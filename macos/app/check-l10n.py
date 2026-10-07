#!/usr/bin/env python3
"""Check French localization: every L("…") key of Sources/ is translated, with the same
number of %@ placeholders, and no stray format directive."""
import plistlib, re, sys, glob, subprocess
keys = set()
for f in glob.glob("Sources/*.swift"):
    for m in re.finditer(r'\bL\("((?:[^"\\]|\\.)*)"', open(f, encoding="utf-8").read()):
        keys.add(m.group(1).replace('\\"', '"'))
fr = plistlib.loads(subprocess.run(["plutil", "-convert", "xml1", "-o", "-", "Resources/fr.lproj/Localizable.strings"],
                                   capture_output=True, check=True).stdout)
errors = [f"missing: {k}" for k in sorted(keys - fr.keys())]
errors += [f"unused: {k}" for k in sorted(fr.keys() - keys)]
for k in keys & fr.keys():
    if k.count("%@") != fr[k].count("%@") or re.search(r"%(?!@)", fr[k]):
        errors.append(f"placeholders: {k} -> {fr[k]}")
print("\n".join(errors) or f"OK: {len(keys)} strings")
sys.exit(1 if errors else 0)
