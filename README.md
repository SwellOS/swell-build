# swell-build

SwellOS build system — builds `.swell` binary packages from source, generates `repo.db`, creates bootable live ISOs.

## Usage

```
swell-build pkg <name>         # Build a single package
swell-build world [-j N]       # Build all packages (parallel, dep-ordered)
swell-build repo               # Generate repo.db from built packages
swell-build list               # List built packages
swell-build info <name>        # Show package details
swell-build clean              # Clean build artifacts + ISO work dir
swell-build iso [OPTIONS]      # Build bootable live ISO
```

### ISO options

```
swell-build iso \
  -K, --kernel   PATH          # Kernel bzImage (auto-searches /boot)
  -R, --root     PATH          # Rootfs dir (builds from .swell packages if omitted)
  -o, --output   PATH          # Output .iso path
      --label    STR           # Volume label (default: SWELLOS_0_1)
  -V, --version  STR           # OS version (default: 0.1)
  -f, --force                  # Overwrite existing files
```

## Build flow

1. `swell-build pkg firefox` reads the package definition from `/usr/src/swell/packages/`
2. Downloads source tarballs to `/usr/src/swell/sources/`
3. Runs `swellbuild` script (bash) to compile
4. Collects installed files from DESTDIR/root/
5. Creates `.swell` archive (tar.zst) with `metadata.toml`, `manifest`, `root/`
6. Output goes to `/usr/src/swell/repo/packages/`

## ISO flow

1. Assembles rootfs from installed `.swell` packages (or accepts `--root`)
2. Creates zstd-compressed squashfs
3. Builds initramfs (busybox + overlay-init script → `switch_root`)
4. Copies kernel, writes GRUB config
5. Runs `grub-mkrescue` for hybrid BIOS/UEFI ISO

## Repo generation

`swell-build repo` scans `/usr/src/swell/repo/packages/` for `.swell` archives and generates `/usr/src/swell/repo/repo.db` (JSON index with dependency metadata).

## Prerequisites

| Tool | Purpose | Package |
|------|---------|---------|
| `bash` | Run build scripts | core |
| `curl` | Download sources | system |
| `tar` + `zstd` | Package archives | system |
| `mksquashfs` | ISO rootfs compression | squashfs-tools |
| `xorriso` | ISO assembly | libisoburn |
| `grub-mkrescue` | Bootloader | grub |
| `busybox` | Initramfs | busybox |

## CI integration

A GitHub Actions workflow can run `swell-build world && swell-build repo` and upload the `repo/` directory to the package server.
