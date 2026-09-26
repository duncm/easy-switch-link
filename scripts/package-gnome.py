#!/usr/bin/env python3
"""Build a deterministic GNOME extension archive without desktop dependencies."""
import json
from pathlib import Path
import re
import zipfile

root = Path(__file__).resolve().parent.parent
version = re.search(r'^version = "([^"]+)"', (root/'Cargo.toml').read_text(), re.M)[1]
source = root/'packaging/gnome/easy-switch-link@duncm.github.com'
assert json.loads((source/'metadata.json').read_text())['version-name'] == version
destination = root/'dist'/f'easy-switch-link-{version}-gnome.shell-extension.zip'
destination.parent.mkdir(exist_ok=True)
# Stored entries avoid differences between host zlib releases. Pin the origin
# platform and normalize source line endings as well as timestamps and modes.
with zipfile.ZipFile(destination, 'w', compression=zipfile.ZIP_STORED) as archive:
    for name in ('extension.js', 'metadata.json'):
        info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
        info.create_system = 3
        info.compress_type = zipfile.ZIP_STORED
        info.external_attr = 0o100644 << 16
        archive.writestr(info, (source/name).read_text(encoding='utf-8').encode('utf-8'))
print(destination)
