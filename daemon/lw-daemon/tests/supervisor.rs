//! Boucle de supervision : géométrie du périphérique, recréation à chaud, mode d'interface.

#![allow(clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use lw_daemon::config::Config;
use lw_daemon::control::Shared;
use lw_daemon::{supervisor, Stop};
use serde_json::{json, Value};

fn call(client: &lw_sys::xpc::Client, req: Value) -> Value {
    serde_json::from_str(&client.call(&req.to_string()).unwrap()).unwrap()
}

/// Attend que `f` soit vrai (2 s au plus).
fn wait(mut f: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn device_geometry_follows_configuration() {
    if !lw_sys::auth::uid_in_group(current_uid(), "admin") {
        eprintln!("ignoré : l'utilisateur du test n'est pas administrateur (commandes de modification refusées)");
        return;
    }
    let shared = Shared::new(None, 0);
    let server = shared.serve(None).unwrap();
    let client = lw_sys::xpc::Client::from_endpoint(&server.endpoint()).unwrap();
    let cfg: Config = serde_json::from_str(
        r#"{"iface":"lo0","advertise":false,"device":{"channels_to_net":2,"channels_from_net":2}}"#,
    )
    .unwrap();
    let stop = Stop::new();
    std::thread::scope(|s| {
        let h = s.spawn(|| {
            supervisor::supervise(&shared, Some(&server), cfg, None, &stop, true)
                .map_err(|e| e.to_string())
        });

        assert!(wait(|| call(&client, json!({"cmd":"geometry"}))
            ["generation"]
            == 1));
        let g = call(&client, json!({"cmd":"geometry"}));
        assert_eq!(
            (
                g["channels_to_net"].as_u64(),
                g["channels_from_net"].as_u64()
            ),
            (Some(2), Some(2))
        );
        assert!(wait(|| call(&client, json!({"cmd":"status"}))["status"]
            ["iface"]
            == "lo0"));

        // Nombre de canaux modifié : nouveau périphérique, génération suivante.
        let r = call(
            &client,
            json!({"cmd":"set_device_channels","to_net":4,"from_net":6}),
        );
        assert_eq!(r["ok"], true, "{r}");
        assert!(wait(|| call(&client, json!({"cmd":"geometry"}))
            ["generation"]
            == 2));
        let g = call(&client, json!({"cmd":"geometry"}));
        assert_eq!(
            (
                g["channels_to_net"].as_u64(),
                g["channels_from_net"].as_u64()
            ),
            (Some(4), Some(6))
        );
        let r = call(
            &client,
            json!({"cmd":"set_device_channels","to_net":40,"from_net":2}),
        );
        assert_eq!(r["ok"], false, "au-delà de 32 canaux : refusé");

        // Mode automatique : lo0 n'est jamais candidate.
        assert_eq!(
            call(&client, json!({"cmd":"set_iface","iface":"auto"}))["ok"],
            true
        );
        assert!(wait(|| {
            let st = call(&client, json!({"cmd":"status"}));
            st["status"]["iface_auto"] == true && st["status"]["iface"] != "lo0"
        }));
        // Un patch seul ne recrée pas le périphérique.
        let r = call(
            &client,
            json!({"cmd":"patch_output","channel":4001,"device_channels":[1,2]}),
        );
        assert_eq!(r["ok"], true, "{r}");
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(call(&client, json!({"cmd":"geometry"}))["generation"], 2);

        stop.request();
        h.join().unwrap().unwrap();
    });
}

/// UID de l'utilisateur courant (sans `unsafe` : lu par la commande `id`).
fn current_uid() -> u32 {
    std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(u32::MAX)
}
