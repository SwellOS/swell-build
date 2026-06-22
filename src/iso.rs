use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub const ISO_WORK_DIR: &str = "/usr/src/swell/iso-work";
const OUTPUT_DIR: &str = "/usr/src/swell/repo";
const DEFAULT_VERSION: &str = "0.1";
const DEFAULT_LABEL: &str = "SWELLOS_0_1";

const INIT_SCRIPT: &str = r#"#!/bin/sh

export PATH=/bin:/sbin:/usr/bin:/usr/sbin

mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null

echo "SwellOS: mounting live media..."
mkdir -p /run/live

label="${SWELL_LABEL:-SWELLOS_0_1}"

for dev in /dev/disk/by-label/"$label" /dev/sr0 /dev/sr1; do
    if [ -e "$dev" ]; then
        mount -r "$dev" /run/live 2>/dev/null && break
    fi
done

if ! mountpoint -q /run/live; then
    echo "SwellOS: waiting for live media..."
    i=0
    while [ $i -lt 10 ]; do
        sleep 1
        for dev in /dev/disk/by-label/"$label" /dev/sr0 /dev/sr1; do
            if [ -e "$dev" ]; then
                mount -r "$dev" /run/live 2>/dev/null && break 2
            fi
        done
        i=$((i + 1))
    done
fi

if ! mountpoint -q /run/live; then
    echo "SwellOS: ERROR - could not find live media"
    exec sh
fi

echo "SwellOS: mounting squashfs..."
mkdir -p /run/root
mount -t squashfs -o loop /run/live/live/filesystem.squashfs /run/root 2>/dev/null

if ! mountpoint -q /run/root; then
    echo "SwellOS: ERROR - could not mount squashfs"
    exec sh
fi

echo "SwellOS: creating writable overlay..."
mkdir -p /run/overlay /run/merged
mount -t tmpfs none /run/overlay
mkdir -p /run/overlay/upper /run/overlay/work
mount -t overlay none -o lowerdir=/run/root,upperdir=/run/overlay/upper,workdir=/run/overlay/work /run/merged

echo "SwellOS: switching to new root..."
mount -o move /dev /run/merged/dev
mount -o move /proc /run/merged/proc
mount -o move /sys /run/merged/sys

exec switch_root /run/merged /sbin/init
"#;

const GRUB_CFG: &str = r#"set default=0
set timeout=5

menuentry "SwellOS" {
    linux /boot/vmlinuz root=live:LABEL=SWELL_LABEL quiet
    initrd /boot/initramfs.gz
}

menuentry "SwellOS (verbose)" {
    linux /boot/vmlinuz root=live:LABEL=SWELL_LABEL
    initrd /boot/initramfs.gz
}
"#;

pub struct IsoOptions {
    pub kernel: Option<String>,
    pub root: Option<String>,
    pub output: Option<String>,
    pub label: Option<String>,
    pub version: Option<String>,
    pub force: bool,
}

pub fn build_iso(opts: IsoOptions) -> Result<(), String> {
    check_iso_prerequisites()?;

    let version = opts.version.as_deref().unwrap_or(DEFAULT_VERSION);
    let label = opts.label.as_deref().unwrap_or(DEFAULT_LABEL);

    let work_dir = PathBuf::from(ISO_WORK_DIR);
    let staging_dir = work_dir.join("staging");

    if staging_dir.exists() {
        if !opts.force {
            return Err(format!(
                "staging dir exists: {}. Use --force to overwrite",
                staging_dir.display()
            ));
        }
        fs::remove_dir_all(&staging_dir)
            .map_err(|e| format!("clean staging: {}", e))?;
    }

    let boot_dir = staging_dir.join("boot");
    let live_dir = staging_dir.join("live");
    let grub_dir = boot_dir.join("grub");

    fs::create_dir_all(&boot_dir)
        .map_err(|e| format!("boot dir: {}", e))?;
    fs::create_dir_all(&live_dir)
        .map_err(|e| format!("live dir: {}", e))?;
    fs::create_dir_all(&grub_dir)
        .map_err(|e| format!("grub dir: {}", e))?;

    let output_path = opts
        .output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(OUTPUT_DIR).join(format!("swellos-{}-x86_64.iso", version)));

    if output_path.exists() && !opts.force {
        return Err(format!(
            "output exists: {}. Use --force to overwrite",
            output_path.display()
        ));
    }

    let start = Instant::now();
    println!("\x1b[1mSwellOS ISO builder v{}\x1b[0m\n", version);

    // 1. Prepare rootfs
    let rootfs_path = match &opts.root {
        Some(p) => {
            let p = PathBuf::from(p);
            if !p.is_dir() {
                return Err(format!("rootfs not found: {}", p.display()));
            }
            println!("  \x1b[36mrootfs\x1b[0m  {}", p.display());
            p
        }
        None => {
            let rootfs = work_dir.join("rootfs");
            if rootfs.exists() {
                fs::remove_dir_all(&rootfs)
                    .map_err(|e| format!("clean rootfs: {}", e))?;
            }
            println!("  \x1b[36mrootfs\x1b[0m  building from .swell packages...");
            assemble_rootfs(&rootfs)?;
            rootfs
        }
    };

    // 2. Create squashfs
    let squashfs_path = live_dir.join("filesystem.squashfs");
    println!("  \x1b[36msquashfs\x1b[0m {}...", squashfs_path.display());
    create_squashfs(&rootfs_path, &squashfs_path)?;

    // 3. Create initramfs
    let initramfs_path = boot_dir.join("initramfs.gz");
    println!("  \x1b[36minitramfs\x1b[0m {}...", initramfs_path.display());
    create_initramfs(&staging_dir, &initramfs_path, label)?;

    // 4. Copy kernel
    let kernel_path = match &opts.kernel {
        Some(p) => PathBuf::from(p),
        None => find_kernel()?,
    };
    if !kernel_path.is_file() {
        return Err(format!("kernel not found: {}", kernel_path.display()));
    }
    println!("  \x1b[36mkernel\x1b[0m   {}", kernel_path.display());
    fs::copy(&kernel_path, boot_dir.join("vmlinuz"))
        .map_err(|e| format!("copy kernel: {}", e))?;

    // 5. Write GRUB config
    let grubcfg = GRUB_CFG.replace("SWELL_LABEL", label);
    fs::write(grub_dir.join("grub.cfg"), &grubcfg)
        .map_err(|e| format!("grub.cfg: {}", e))?;

    // 6. Assemble ISO
    println!("  \x1b[36miso\x1b[0m     {}...", output_path.display());
    assemble_iso(&staging_dir, &output_path, label)?;

    let elapsed = start.elapsed();
    let size = fs::metadata(&output_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let size_mb = size as f64 / 1048576.0;

    println!(
        "\n\x1b[32m✓\x1b[0m ISO created: {} ({:.1} MB, {}.{:02}s)",
        output_path.display(),
        size_mb,
        elapsed.as_secs(),
        elapsed.subsec_millis() / 10,
    );

    // Cleanup staging to save space
    fs::remove_dir_all(&staging_dir).ok();

    Ok(())
}

fn check_iso_prerequisites() -> Result<(), String> {
    let tools = &["mksquashfs", "xorriso", "grub-mkrescue", "cpio", "gzip", "find"];
    for tool in tools {
        let status = Command::new("which")
            .arg(tool)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|_| format!("failed to check for {}", tool))?;
        if !status.success() {
            return Err(format!(
                "required tool not found: {} (install squashfs-tools, libisoburn, grub, cpio)",
                tool
            ));
        }
    }
    Ok(())
}

fn find_kernel() -> Result<PathBuf, String> {
    // Look in standard locations
    let candidates = &[
        "/boot/vmlinuz-linux-swell",
        "/boot/vmlinuz-swell",
        "/boot/vmlinuz-6.x-swell",
        "/boot/vmlinuz",
        "/usr/src/swell/repo/iso/bzImage",
        "/usr/src/swell/kernel/arch/x86_64/boot/bzImage",
    ];
    for c in candidates {
        let p = Path::new(c);
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
    }
    Err("no kernel found. Build the Swell kernel first or pass --kernel".to_string())
}

fn assemble_rootfs(rootfs: &Path) -> Result<(), String> {
    let packages_dir = PathBuf::from(OUTPUT_DIR).join("packages");
    let work_dir = PathBuf::from(ISO_WORK_DIR);
    if !packages_dir.is_dir() {
        return Err(format!(
            "no built packages in {}. Run 'swell-build world' first or pass --root",
            packages_dir.display()
        ));
    }

    let entries = fs::read_dir(&packages_dir)
        .map_err(|e| format!("read packages: {}", e))?;

    let mut archives: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "swell") {
            archives.push(path);
        }
    }

    if archives.is_empty() {
        return Err("no .swell archives found in packages dir".to_string());
    }

    // Sort by name to ensure consistent install order
    archives.sort();

    // Create necessary directories
    for dir in &["dev", "proc", "sys", "run", "tmp", "var/lib", "var/log", "etc/sv"] {
        fs::create_dir_all(rootfs.join(dir)).ok();
    }

    for archive in &archives {
        let filename = archive.file_name().unwrap().to_string_lossy();

        // Extract to a temp dir
        let extract_dir = work_dir.join("extract");
        if extract_dir.exists() {
            fs::remove_dir_all(&extract_dir).ok();
        }
        fs::create_dir_all(&extract_dir)
            .map_err(|e| format!("extract dir: {}", e))?;

        let status = Command::new("tar")
            .args(["--zstd", "-xf", &archive.to_string_lossy(), "-C", &extract_dir.to_string_lossy()])
            .status()
            .map_err(|e| format!("extract {}: {}", filename, e))?;

        if !status.success() {
            return Err(format!("failed to extract {}", filename));
        }

        // Copy root/ contents to rootfs
        let root_src = extract_dir.join("root");
        if root_src.is_dir() {
            copy_dir_all(&root_src, rootfs)?;
        }

        fs::remove_dir_all(&extract_dir).ok();

        print!(".");
    }

    println!();

    // Generate /etc/fstab
    let fstab = "# /etc/fstab: static filesystem information
# Live ISO — filesystems are mounted by initramfs
";
    fs::write(rootfs.join("etc/fstab"), fstab)
        .map_err(|e| format!("fstab: {}", e))?;

    Ok(())
}

fn create_squashfs(rootfs: &Path, output: &Path) -> Result<(), String> {
    // Remove old squashfs
    if output.exists() {
        fs::remove_file(output).ok();
    }

    let status = Command::new("mksquashfs")
        .args([
            &rootfs.to_string_lossy(),
            &output.to_string_lossy(),
            "-noappend",
            "-comp", "zstd",
            "-b", "1M",
        ])
        .status()
        .map_err(|e| format!("mksquashfs: {}", e))?;

    if !status.success() {
        return Err("squashfs creation failed".to_string());
    }

    Ok(())
}

fn create_initramfs(staging_dir: &Path, output: &Path, label: &str) -> Result<(), String> {
    let initrd_dir = staging_dir.join("initrd-work");

    if initrd_dir.exists() {
        fs::remove_dir_all(&initrd_dir).ok();
    }

    // Create directory structure
    let dirs = &["bin", "dev", "proc", "sys", "run", "etc", "sbin"];
    for d in dirs {
        fs::create_dir_all(initrd_dir.join(d))
            .map_err(|e| format!("initrd {}: {}", d, e))?;
    }

    // Write init script
    let init_script = INIT_SCRIPT.replace("SWELL_LABEL", label);
    fs::write(initrd_dir.join("init"), &init_script)
        .map_err(|e| format!("init script: {}", e))?;

    // Make init executable
    set_permissions(&initrd_dir.join("init"), 0o755)?;

    // Copy busybox (must be pre-built or available on host)
    let busybox_src = find_busybox()?;
    fs::copy(&busybox_src, initrd_dir.join("bin/busybox"))
        .map_err(|e| format!("copy busybox: {}", e))?;

    // Create busybox symlinks
    let applets = &["sh", "mount", "umount", "sleep", "cat", "echo",
                     "ls", "mkdir", "switch_root", "grep",
                     "mountpoint", "dmesg", "insmod", "modprobe"];
    for applet in applets {
        let link_path = initrd_dir.join("bin").join(applet);
        if !link_path.exists() {
            std::os::unix::fs::symlink("../bin/busybox", &link_path)
                .map_err(|e| format!("symlink {}: {}", applet, e))?;
        }
    }

    // Create /dev/console and /dev/null if not present
    if !initrd_dir.join("dev/console").exists() {
        Command::new("mknod")
            .args([&initrd_dir.join("dev/console").to_string_lossy(), "c", "5", "1"])
            .status()
            .map_err(|e| format!("mknod console: {}", e))?;
    }
    if !initrd_dir.join("dev/null").exists() {
        Command::new("mknod")
            .args([&initrd_dir.join("dev/null").to_string_lossy(), "c", "1", "3"])
            .status()
            .map_err(|e| format!("mknod null: {}", e))?;
    }

    // Package as gzipped cpio archive
    let status = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "cd {} && find . | cpio -H newc -o | gzip -9 > {}",
            initrd_dir.to_string_lossy(),
            output.to_string_lossy()
        ))
        .status()
        .map_err(|e| format!("cpio/gzip: {}", e))?;

    if !status.success() {
        return Err("initramfs creation failed".to_string());
    }

    // Cleanup
    fs::remove_dir_all(&initrd_dir).ok();

    Ok(())
}

fn find_busybox() -> Result<PathBuf, String> {
    let candidates = &[
        "/bin/busybox",
        "/usr/bin/busybox",
        "/usr/src/swell/repo/iso/busybox",
        "/usr/src/swell/build/busybox/busybox",
    ];
    for c in candidates {
        let p = Path::new(c);
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
    }
    Err("busybox not found. Build busybox first or install it on the host".to_string())
}

fn assemble_iso(staging_dir: &Path, output: &Path, label: &str) -> Result<(), String> {
    // Remove old ISO
    if output.exists() {
        fs::remove_file(output).ok();
    }

    let status = Command::new("grub-mkrescue")
        .args([
            "-o",
            &output.to_string_lossy(),
            &staging_dir.to_string_lossy(),
            "--",
            "-volid",
            label,
            "-volset",
            label,
            "-appid",
            "SwellOS",
            "-publisher",
            "SwellOS",
            "-sysid",
            "x86_64",
        ])
        .status()
        .map_err(|e| format!("grub-mkrescue: {}", e))?;

    if !status.success() {
        return Err("ISO assembly failed".to_string());
    }

    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    if !dst.exists() {
        fs::create_dir_all(dst)
            .map_err(|e| format!("create dir {}: {}", dst.display(), e))?;
    }

    let entries = fs::read_dir(src)
        .map_err(|e| format!("read dir {}: {}", src.display(), e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(src)
            .map_err(|_| "strip_prefix failed".to_string())?;
        let target = dst.join(relative);

        let meta = fs::metadata(&path)
            .map_err(|e| format!("metadata {}: {}", path.display(), e))?;

        if meta.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|e| format!("create dir {}: {}", target.display(), e))?;
            copy_dir_all(&path, &target)?;
        } else if meta.is_symlink() {
            let link = fs::read_link(&path)
                .map_err(|e| format!("readlink {}: {}", path.display(), e))?;
            std::os::unix::fs::symlink(&link, &target)
                .map_err(|e| format!("symlink {}: {}", target.display(), e))?;
        } else {
            fs::copy(&path, &target)
                .map_err(|e| format!("copy {}: {}", target.display(), e))?;
            // Preserve permissions
            if let Ok(perm) = fs::metadata(&path).map(|m| m.permissions()) {
                fs::set_permissions(&target, perm).ok();
            }
        }
    }

    Ok(())
}

fn set_permissions(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("chmod {}: {}", path.display(), e))
}
