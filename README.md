# swell-build

SwellOS build system — builds `.swell` binary packages from source, generates `repo.db`.

## Usage

```
swell-build pkg <name>   # Build a package from source
swell-build world        # Build all packages
swell-build repo         # Generate repo.db from built packages
swell-build list         # List built packages
swell-build clean        # Clean build artifacts
```

## Build flow

1. `swell-build pkg firefox` reads the package definition from `/usr/src/swell/packages/`
2. Downloads source tarballs to `/usr/src/swell/sources/`
3. Runs `swellbuild` script (bash) to compile
4. Collects installed files from DESTDIR
5. Creates `.swell` archive (tar.zst) with `metadata.toml`, `manifest`, `root/`
6. Output goes to `/usr/src/swell/repo/packages/`

## Repo generation

`swell-build repo` scans `/usr/src/swell/repo/packages/` for `.swell` archives and generates `/usr/src/swell/repo/repo.db` (JSON index).

## CI integration

A GitHub Actions workflow can run `swell-build world && swell-build repo` and upload the `repo/` directory to the package server.
