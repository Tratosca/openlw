#!/usr/bin/env python3
"""Generate (RFC 4566 / RFC 7273) and validate AES67 SDP.

    python3 sdptool.py gen --channel 101 --host 192.168.10.20 --name "Mac 1" [--gm 00-1D-C1-FF-FE-12-34-56 --domain 0]
    python3 sdptool.py check file.sdp
"""
import argparse
import re
import sys

from lwchan import channel_to_group


def ptime_text(samples, rate=48000):
    """Driver rule: multiple of 48 -> integer (ms); 60 or 12 -> %3.2f; otherwise '1'."""
    if samples % 48 == 0:
        return str(samples * 1000 // rate)
    if samples in (60, 12):
        return f"{samples * 1000 / rate:3.2f}"
    return "1"


def generate(group, host, name, port=5004, pt=96, bits=24, rate=48000, channels=2, samples=48,
             direction="sendonly", ttl=None, gm=None, domain=0, sess_id=0, sess_ver=0, maxptime_samples=None):
    lines = [
        "v=0",
        f"o=- {sess_id} {sess_ver} IN IP4 {host}",
        f"s={name}",
        f"c=IN IP4 {group}" + (f"/{ttl}" if ttl else ""),
        "t=0 0",
        f"m=audio {port} RTP/AVP {pt}",
        f"a=rtpmap:{pt} L{bits}/{rate}/{channels}",
        f"a={direction}",
        f"a=ptime:{ptime_text(samples, rate)}",
    ]
    if maxptime_samples:
        lines.append(f"a=maxptime:{ptime_text(maxptime_samples, rate)}")
    if gm:
        lines.append(f"a=ts-refclk:ptp=IEEE1588-2008:{gm.upper()}:{domain}")
        lines.append("a=mediaclk:direct=0")
    return "\r\n".join(lines) + "\r\n"


def check(text):
    """Return (info, problems). Validate AES67 receiver requirements."""
    problems, info = [], {}
    lines = [l.strip() for l in text.replace("\r\n", "\n").split("\n") if l.strip()]
    if not lines or lines[0] != "v=0":
        problems.append("premiere ligne differente de v=0")
    for line in lines:
        if line.startswith("c=IN IP4 "):
            info["group"] = line[9:].split("/")[0]
        elif line.startswith("m=audio "):
            parts = line.split()
            info["port"], info["pts"] = int(parts[1]), parts[3:]
        elif line.startswith("a=rtpmap:"):
            m = re.match(r"a=rtpmap:(\d+) L(\d+)/(\d+)/(\d+)", line)
            if m:
                info["rtpmap"] = tuple(int(x) for x in m.groups())
            else:
                problems.append(f"rtpmap non lineaire ou incomplet : {line}")
        elif line.startswith("a=ptime:"):
            info["ptime"] = float(line[8:])
        elif line.startswith("a=ts-refclk:"):
            info["refclk"] = line[12:]
        elif line.startswith("a=mediaclk:"):
            info["mediaclk"] = line[11:]
    for key in ("group", "port", "rtpmap"):
        if key not in info:
            problems.append(f"{key} absent")
    if "refclk" not in info:
        problems.append("a=ts-refclk absent (pas de reference PTP ; pas de grandmaster)")
    if "mediaclk" not in info:
        problems.append("a=mediaclk absent")
    if "rtpmap" in info and info["rtpmap"][2] != 48000:
        problems.append("frequence differente de 48000")
    return info, problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    g = sub.add_parser("gen")
    g.add_argument("--channel", type=int, required=True)
    g.add_argument("--host", required=True)
    g.add_argument("--name", required=True)
    g.add_argument("--pt", type=int, default=96)
    g.add_argument("--samples", type=int, default=48)
    g.add_argument("--channels", type=int, default=2)
    g.add_argument("--gm", help="identite du grandmaster, ex. 00-1D-C1-FF-FE-12-34-56")
    g.add_argument("--domain", type=int, default=0)
    c = sub.add_parser("check")
    c.add_argument("file")
    args = parser.parse_args(argv)
    if args.cmd == "gen":
        kind = "surround" if args.channels == 8 else "stereo"
        sys.stdout.write(generate(channel_to_group(args.channel, kind), args.host, args.name, pt=args.pt,
                                  samples=args.samples, channels=args.channels, gm=args.gm, domain=args.domain))
        return 0
    info, problems = check(open(args.file, encoding="latin-1").read())
    print(info)
    for p in problems:
        print("ATTENTION :", p)
    return 1 if any("absent" in p and "refclk" not in p and "mediaclk" not in p for p in problems) else 0


if __name__ == "__main__":
    sys.exit(main())
