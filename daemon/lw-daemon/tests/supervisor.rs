//! Supervision loop: device geometry, live recreation, interface mode.

#![allow(clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use lw_daemon::config::Config;
use lw_daemon::control::Shared;
use lw_daemon::{iface, supervisor, Stop};
use lw_sys::ctl::{Client, Endpoint};
use serde_json::{json, Value};

fn call(client: &Client, req: Value) -> Value {
    serde_json::from_str(&client.call(&req.to_string()).unwrap()).unwrap()
}

/// Test-specific control channel: temporary Unix socket or unique named pipe.
fn test_endpoint() -> Endpoint {
    #[cfg(unix)]
    return Endpoint::Socket(
        std::env::temp_dir().join(format!("openlw-supervisor-{}.sock", std::process::id())),
    );
    #[cfg(windows)]
    return Endpoint::Pipe(format!(
        "fr.francois-brille.openlw.supervisor-test.{}",
        std::process::id()
    ));
}

/// Wait for `f` to become true (at most 2 s).
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
    let lo = iface::list()
        .unwrap()
        .into_iter()
        .find(|i| i.loopback)
        .expect("loopback interface");
    let shared = Shared::new(None, 0);
    let ep = test_endpoint();
    let server = shared.serve(&ep).unwrap();
    let client = Client::connect(&ep).unwrap();
    let cfg: Config = serde_json::from_value(json!({
        "iface": lo.name, "advertise": false,
        "device": {"channels_to_net": 2, "channels_from_net": 2}
    }))
    .unwrap();
    let stop = Stop::new();
    std::thread::scope(|s| {
        // A failed assertion stops the supervisor too (otherwise the scope waits forever).
        struct StopOnDrop<'a>(&'a Stop);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                self.0.request();
            }
        }
        let _guard = StopOnDrop(&stop);
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
            == lo.name.as_str()));

        // Modification commands require edit privileges (see ctl::edit_policy).
        let probe = call(&client, json!({"cmd":"set_advertise","advertise":false}));
        if probe["ok"] != true {
            eprintln!("skipped: changes refused for the test user ({probe})");
            stop.request();
            h.join().unwrap().unwrap();
            return;
        }

        // Channel count changed: new device, next generation.
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
        assert_eq!(r["ok"], false, "more than 32 channels: refused");

        // Automatic mode: loopback interface is never a candidate.
        assert_eq!(
            call(&client, json!({"cmd":"set_iface","iface":"auto"}))["ok"],
            true
        );
        assert!(wait(|| {
            let st = call(&client, json!({"cmd":"status"}));
            st["status"]["iface_auto"] == true && st["status"]["iface"] != lo.name.as_str()
        }));
        // A patch alone does not recreate the device.
        let r = call(
            &client,
            json!({"cmd":"patch_output","channel":4001,"device_channels":[1,2]}),
        );
        assert_eq!(r["ok"], true, "{r}");
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(call(&client, json!({"cmd":"geometry"}))["generation"], 2);

        // Multi layout (macOS and Linux): one ring per device; a patch changing a device's width
        // recreates the region, a stereo patch on an empty device does not.
        let r = call(&client, json!({"cmd":"set_device_layout","layout":"multi"}));
        if !cfg!(windows) {
            assert_eq!(r["ok"], true, "{r}");
            assert!(wait(|| call(&client, json!({"cmd":"geometry"}))
                ["generation"]
                == 3));
            let g = call(&client, json!({"cmd":"geometry"}));
            assert_eq!(g["layout"], "multi");
            assert_eq!(g["in_widths"], json!([2, 2, 2]));
            assert_eq!(g["out_widths"], json!([2, 2]), "output patch 1-2 → Out 1");
            assert_eq!(
                g["out_device_names"][0],
                "OpenLW Out - PC (ch. 4001)".replace("PC", lw_daemon::config::DEFAULT_SOURCE_NAME)
            );
            let r = call(
                &client,
                json!({"cmd":"patch_input","channel":21,"device":1,"device_channels":[1,2]}),
            );
            assert_eq!(r["ok"], true, "{r}");
            std::thread::sleep(Duration::from_millis(500));
            assert_eq!(call(&client, json!({"cmd":"geometry"}))["generation"], 3);
            // L + R on a coupled device: no width change.
            let r = call(
                &client,
                json!({"cmd":"patch_input","channel":22,"taps":[{"device":2,"channel":1,"from":[1,2]}]}),
            );
            assert_eq!(r["ok"], true, "{r}");
            std::thread::sleep(Duration::from_millis(500));
            assert_eq!(call(&client, json!({"cmd":"geometry"}))["generation"], 3);
            // Uncoupled device: mono, new region.
            let r = call(
                &client,
                json!({"cmd":"set_coupling","pair":2,"coupled":false}),
            );
            assert_eq!(r["ok"], true, "{r}");
            assert!(wait(|| call(&client, json!({"cmd":"geometry"}))
                ["generation"]
                == 4));
            let g = call(&client, json!({"cmd":"geometry"}));
            assert_eq!(g["in_widths"], json!([2, 1, 2]));
            assert_eq!(g["channels_from_net"], 5);
        } else {
            assert_eq!(r["ok"], false, "multi layout refused on Windows");
        }

        stop.request();
        h.join().unwrap().unwrap();
    });
}
