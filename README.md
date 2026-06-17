# swell-build

SwellOS build system — ISO generator, kernel rebuild, world rebuild.

## Usage

```bash
swell-build kernel     # rebuild the Swell kernel
swell-build glibc      # rebuild glibc
swell-build world      # rebuild all installed packages
swell-build iso        # generate a live ISO from current system
swell-build clean      # clean build artifacts
```

Every build produces a manifest at `/usr/src/swell/manifests/<package>-<version>` listing every installed file and its checksum.
