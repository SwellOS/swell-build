mod iso;

use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const PACKAGES_REPO: &str = "/usr/src/swell/packages";
const OUTPUT_DIR: &str = "/usr/src/swell/repo";
const BUILD_DIR: &str = "/usr/src/swell/build";
const SOURCE_DIR: &str = "/usr/src/swell/sources";
const DESTDIR_BASE: &str = "/usr/src/swell/dest";

const CATEGORIES: &[&str] = &[
    "core", "desktop", "browser", "dev", "gaming", "multimedia", "office", "network", "community",
];

#[derive(Parser)]
#[command(name = "swell-build", about = "SwellOS build system")]
struct Cli {
    #[arg(short = 'j', long = "jobs", default_value = "1")]
    jobs: usize,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Build a package from source
    Pkg {
        name: String,
    },
    /// Build all packages (in dependency order)
    World,
    /// Generate repo.db from built packages
    Repo,
    /// Clean build artifacts
    Clean,
    /// List built packages
    List,
    /// Show info about a built package
    Info {
        name: String,
    },
    /// Build a bootable live ISO
    Iso {
        /// Path to kernel bzImage
        #[arg(long, short = 'K')]
        kernel: Option<String>,
        /// Path to rootfs directory (builds from .swell packages if omitted)
        #[arg(long, short = 'R')]
        root: Option<String>,
        /// Output ISO path
        #[arg(long, short = 'o')]
        output: Option<String>,
        /// ISO volume label
        #[arg(long)]
        label: Option<String>,
        /// OS version string
        #[arg(long, short = 'V')]
        version: Option<String>,
        /// Overwrite existing ISO and staging
        #[arg(long, short = 'f')]
        force: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Pkg { name } => {
            if let Err(e) = build_package(name, true) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
        Commands::World => {
            if let Err(e) = build_all(cli.jobs) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
        Commands::Repo => {
            if let Err(e) = generate_repo_db() {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
        Commands::Clean => {
            clean_all();
        }
        Commands::List => {
            list_built();
        }
        Commands::Info { name } => {
            if let Err(e) = info_package(name) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
        Commands::Iso {
            kernel,
            root,
            output,
            label,
            version,
            force,
        } => {
            let opts = iso::IsoOptions {
                kernel: kernel.clone(),
                root: root.clone(),
                output: output.clone(),
                label: label.clone(),
                version: version.clone(),
                force: *force,
            };
            if let Err(e) = iso::build_iso(opts) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    }
}

fn check_tools() -> Result<(), String> {
    for tool in &["bash", "curl", "tar", "zstd", "sha256sum"] {
        let status = Command::new("which")
            .arg(tool)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|_| format!("failed to check for {}", tool))?;
        if !status.success() {
            return Err(format!("required tool not found: {}", tool));
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct PackageDef {
    name: String,
    category: String,
    path: PathBuf,
    version: String,
    depends: Vec<String>,
    sources: Vec<String>,
}

fn load_package_def(name: &str) -> Result<PackageDef, String> {
    for cat in CATEGORIES {
        let candidate = Path::new(PACKAGES_REPO).join(cat).join(name);
        if candidate.is_dir() {
            let version = fs::read_to_string(candidate.join("version"))
                .map_err(|_| format!("no version file for {}", name))?
                .trim()
                .to_string();

            let depends = if candidate.join("depends").exists() {
                fs::read_to_string(candidate.join("depends"))
                    .unwrap_or_default()
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            } else {
                Vec::new()
            };

            let sources = if candidate.join("sources").exists() {
                fs::read_to_string(candidate.join("sources"))
                    .unwrap_or_default()
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            } else {
                Vec::new()
            };

            return Ok(PackageDef {
                name: name.to_string(),
                category: cat.to_string(),
                path: candidate,
                version,
                depends,
                sources,
            });
        }
    }
    Err(format!("package not found: {}", name))
}

fn resolve_build_order(packages: &[PackageDef]) -> Result<Vec<String>, String> {
    let mut graph: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut all_pkgs: HashSet<&str> = HashSet::new();

    for pkg in packages {
        all_pkgs.insert(pkg.name.as_str());
    }

    for pkg in packages {
        let deps: Vec<&str> = pkg
            .depends
            .iter()
            .map(|d| d.as_str())
            .filter(|d| all_pkgs.contains(d))
            .collect();
        graph.insert(pkg.name.as_str(), deps);
    }

    let mut visited = HashSet::new();
    let mut stack = HashSet::new();
    let mut order = Vec::new();

    fn visit(
        name: &str,
        graph: &HashMap<&str, Vec<&str>>,
        visited: &mut HashSet<String>,
        stack: &mut HashSet<String>,
        order: &mut Vec<String>,
    ) -> Result<(), String> {
        if stack.contains(name) {
            return Err(format!("dependency cycle detected: {}", name));
        }
        if visited.contains(name) {
            return Ok(());
        }
        stack.insert(name.to_string());
        if let Some(deps) = graph.get(name) {
            for dep in deps {
                visit(dep, graph, visited, stack, order)?;
            }
        }
        stack.remove(name);
        visited.insert(name.to_string());
        order.push(name.to_string());
        Ok(())
    }

    for pkg in packages {
        visit(pkg.name.as_str(), &graph, &mut visited, &mut stack, &mut order)?;
    }

    Ok(order)
}

fn build_package(name: &str, show_progress: bool) -> Result<(), String> {
    check_tools()?;

    let pkg = load_package_def(name)?;

    if !pkg.path.join("swellbuild").exists() {
        return Err(format!("no swellbuild script for {} (metapackage?)", name));
    }

    let src_dir = PathBuf::from(SOURCE_DIR).join(&pkg.name);
    let build_dir = PathBuf::from(BUILD_DIR).join(&pkg.name);
    let dest_dir = PathBuf::from(DESTDIR_BASE).join(&pkg.name);
    let root_dir = dest_dir.join("root");

    if build_dir.exists() {
        fs::remove_dir_all(&build_dir)
            .map_err(|e| format!("clean build dir: {}", e))?;
    }
    if dest_dir.exists() {
        fs::remove_dir_all(&dest_dir)
            .map_err(|e| format!("clean dest dir: {}", e))?;
    }

    fs::create_dir_all(&src_dir)
        .map_err(|e| format!("source dir: {}", e))?;
    fs::create_dir_all(&build_dir)
        .map_err(|e| format!("build dir: {}", e))?;
    fs::create_dir_all(&root_dir)
        .map_err(|e| format!("root dir: {}", e))?;

    let start = Instant::now();

    if show_progress {
        println!("  \x1b[1mBuilding {} {}\x1b[0m", pkg.name, pkg.version);
    }

    // Download sources
    for url in &pkg.sources {
        let filename = url.split('/').last().unwrap_or("unknown");
        let dest = src_dir.join(filename);
        if !dest.exists() {
            if show_progress {
                println!("    \x1b[36mfetching\x1b[0m {}", url);
            }
            let status = Command::new("curl")
                .args(["-L", "-o", &dest.to_string_lossy(), url])
                .status()
                .map_err(|e| format!("curl failed: {}", e))?;
            if !status.success() {
                return Err(format!("failed to download {}", url));
            }
        } else if show_progress {
            println!("    \x1b[36mcached\x1b[0m  {}", filename);
        }
    }

    // Build helper library
    let sbpm_lib = format!(
        r#"
DESTDIR="{root}"
SRCDIR="{src}"

fetch_url() {{
    echo "fetch_url: $1"
}}

unpack() {{
    local archive="$1"
    case "$archive" in
        *.tar.gz|*.tgz) tar xzf "{src}/$archive" ;;
        *.tar.xz)       tar xJf "{src}/$archive" ;;
        *.tar.bz2)      tar xjf "{src}/$archive" ;;
        *.tar)          tar xf "{src}/$archive" ;;
        *.zip)          unzip "{src}/$archive" ;;
        *)              echo "Unknown archive: $archive"; return 1 ;;
    esac
}}
"#,
        root = root_dir.display(),
        src = src_dir.display(),
    );

    let script = format!(
        r#"set -e
{lib}
source "{swellbuild}"

if declare -f pkg_fetch > /dev/null; then
    cd "{src}"
    pkg_fetch
fi

if declare -f pkg_unpack > /dev/null; then
    cd "{src}"
    pkg_unpack
fi

# Enter the first subdirectory (unpacked source)
cd "{src}"
for d in "$SRCDIR"/*/; do
    if [ -d "$d" ]; then
        cd "$d"
        break
    fi
done

# If the swellbuild script sets srcdir, use that instead
if [ -n "${{srcdir:-}}" ] && [ -d "$srcdir" ]; then
    cd "$srcdir"
fi

if declare -f pkg_build > /dev/null; then
    pkg_build
fi

if declare -f pkg_install > /dev/null; then
    pkg_install
fi
"#,
        lib = sbpm_lib,
        swellbuild = pkg.path.join("swellbuild").display(),
        src = src_dir.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(&build_dir)
        .output()
        .map_err(|e| format!("failed to run build: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "\x1b[31m BUILD FAILED: {}\x1b[0m\n--- STDOUT ---\n{}\n--- STDERR ---\n{}",
            pkg.name, stdout, stderr
        ));
    }

    // Collect installed files
    let mut installed_files = Vec::new();
    collect_files(&root_dir, &root_dir, &mut installed_files);

    // Write metadata.toml
    let depends_str = if pkg.depends.is_empty() {
        String::new()
    } else {
        format!("\ndepends = [{}]\n",
            pkg.depends.iter()
                .map(|d| format!("\"{}\"", d))
                .collect::<Vec<_>>()
                .join(", "))
    };

    let metadata = format!(
        r#"name = "{}"
version = "{}"
release = 1
arch = "x86_64"
{}"#,
        pkg.name, pkg.version, depends_str
    );
    fs::write(dest_dir.join("metadata.toml"), &metadata)
        .map_err(|e| format!("metadata: {}", e))?;

    // Write manifest
    let manifest_content = installed_files.join("\n");
    fs::write(dest_dir.join("manifest"), &manifest_content)
        .map_err(|e| format!("manifest: {}", e))?;

    // Create output directory
    let output_dir = PathBuf::from(OUTPUT_DIR).join("packages");
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("output dir: {}", e))?;

    let archive_name = format!("{}-{}-1-x86_64.swell", pkg.name, pkg.version);
    let archive_path = output_dir.join(&archive_name);

    // Remove old archive for this package
    if archive_path.exists() {
        fs::remove_file(&archive_path).ok();
    }

    // Create .swell archive
    let status = Command::new("tar")
        .args([
            "--zstd", "-cf",
            &archive_path.to_string_lossy(),
            "-C", &dest_dir.to_string_lossy(),
            "metadata.toml", "manifest", "root",
        ])
        .status()
        .map_err(|e| format!("tar failed: {}", e))?;

    if !status.success() {
        return Err("failed to create archive".to_string());
    }

    let elapsed = start.elapsed();
    let size = fs::metadata(&archive_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let size_kb = size / 1024;

    if show_progress {
        println!(
            "    \x1b[32m✓\x1b[0m {} {}.swell ({} KB, {}.{:02}s)",
            pkg.name,
            pkg.version,
            size_kb,
            elapsed.as_secs(),
            elapsed.subsec_millis() / 10,
        );
    }

    // Cleanup build directory to save space
    fs::remove_dir_all(&build_dir).ok();

    Ok(())
}

fn build_all(jobs: usize) -> Result<(), String> {
    check_tools()?;

    // Collect all packages with swellbuild scripts
    let mut packages = Vec::new();
    for cat in CATEGORIES {
        let cat_path = Path::new(PACKAGES_REPO).join(cat);
        if !cat_path.is_dir() {
            continue;
        }
        let entries = fs::read_dir(&cat_path)
            .map_err(|e| format!("read dir {}: {}", cat, e))?;
        for entry in entries.flatten() {
            let pkg_path = entry.path();
            if pkg_path.is_dir() {
                if let Some(name) = pkg_path.file_name().and_then(|n| n.to_str()) {
                    if pkg_path.join("swellbuild").exists() {
                        match load_package_def(name) {
                            Ok(def) => packages.push(def),
                            Err(e) => eprintln!("  warning: skipping {}: {}", name, e),
                        }
                    }
                }
            }
        }
    }

    if packages.is_empty() {
        return Err("no packages found to build".to_string());
    }

    // Resolve build order (topological sort)
    let order = resolve_build_order(&packages)?;

    println!(
        "\x1b[1mBuilding {} packages in dependency order ({} jobs)\x1b[0m\n",
        order.len(),
        jobs
    );

    if jobs <= 1 {
        // Sequential build
        let total = order.len();
        for (i, name) in order.iter().enumerate() {
            print!("\x1b[33m[{}/{}]\x1b[0m ", i + 1, total);
            if let Err(e) = build_package(name, true) {
                eprintln!("{}", e);
                // Continue building other packages
            }
        }
    } else {
        // Parallel build with topological ordering
        let built = Arc::new(Mutex::new(HashSet::new()));
        let queue = Arc::new(Mutex::new(order.clone()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let total = order.len();
        let completed = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..jobs {
            let queue = queue.clone();
            let built = built.clone();
            let errors = errors.clone();
            let active = active.clone();
            let completed = completed.clone();

            handles.push(std::thread::spawn(move || loop {
                let name = {
                    let mut q = queue.lock().unwrap();
                    // Find next package whose deps are all built
                    let mut idx = None;
                    for (i, name) in q.iter().enumerate() {
                        if let Ok(pkg) = load_package_def(name) {
                            let built_set = built.lock().unwrap();
                            let deps_met = pkg.depends.iter().all(|d| built_set.contains(d));
                            if deps_met {
                                idx = Some(i);
                                break;
                            }
                        }
                    }
                    match idx {
                        Some(i) => {
                            let name = q.remove(i);
                            active.fetch_add(1, Ordering::SeqCst);
                            Some(name)
                        }
                        None => None,
                    }
                };

                match name {
                    Some(name) => {
                        let i = completed.fetch_add(1, Ordering::SeqCst) + 1;
                        print!("\x1b[33m[{}/{}]\x1b[0m ", i, total);
                        if let Err(e) = build_package(&name, true) {
                            eprintln!("{}", e);
                            let mut errs = errors.lock().unwrap();
                            errs.push(format!("{}: {}", name, e));
                        }
                        built.lock().unwrap().insert(name);
                        active.fetch_sub(1, Ordering::SeqCst);
                    }
                    None => {
                        let remaining = queue.lock().unwrap().len();
                        if remaining == 0 && active.load(Ordering::SeqCst) == 0 {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let errs = errors.lock().unwrap();
        if !errs.is_empty() {
            println!("\n\x1b[31mBuild errors:\x1b[0m");
            for e in errs.iter() {
                println!("  {}", e);
            }
        }
    }

    println!("\n\x1b[1mBuild complete.\x1b[0m");
    Ok(())
}

fn generate_repo_db() -> Result<(), String> {
    let packages_dir = PathBuf::from(OUTPUT_DIR).join("packages");
    if !packages_dir.exists() {
        return Err("no packages built yet".to_string());
    }

    let mut db: HashMap<String, serde_json::Value> = HashMap::new();

    let entries = fs::read_dir(&packages_dir)
        .map_err(|e| format!("read packages: {}", e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "swell") {
            let filename = path.file_name().unwrap().to_string_lossy().to_string();
            let metadata = fs::metadata(&path)
                .map_err(|e| format!("metadata: {}", e))?;
            let size = metadata.len();
            let sha = sha256_file(&path);

            // Parse name-version-release-arch.swell
            let stem = filename.strip_suffix(".swell").unwrap_or(&filename);
            let parts: Vec<&str> = stem.rsplitn(4, '-').collect();
            if parts.len() >= 4 {
                let arch = parts[0];
                let release: u32 = parts[1].parse().unwrap_or(1);
                let version = parts[2];
                let name = parts[3];

                // Try to extract depends from the archive
                let depends = extract_depends_from_archive(&path);

                let entry = serde_json::json!({
                    "version": version,
                    "release": release,
                    "arch": arch,
                    "depends": depends,
                    "size": size,
                    "sha256": sha,
                    "url": format!("packages/{}", filename)
                });

                db.insert(name.to_string(), entry);
            }
        }
    }

    let repo_json = serde_json::to_string_pretty(&db)
        .map_err(|e| format!("serialize: {}", e))?;

    let repo_path = PathBuf::from(OUTPUT_DIR).join("repo.db");
    fs::write(&repo_path, &repo_json)
        .map_err(|e| format!("write repo.db: {}", e))?;

    println!("\x1b[1mRepo DB:\x1b[0m {} ({} packages)", repo_path.display(), db.len());
    Ok(())
}

fn extract_depends_from_archive(path: &Path) -> Vec<String> {
    // Extract metadata.toml from the .swell archive and parse depends
    let output = Command::new("tar")
        .args([
            "--zstd", "-xOf",
            &path.to_string_lossy(),
            "metadata.toml",
        ])
        .output()
        .ok();

    match output {
        Some(out) if out.status.success() => {
            let content = String::from_utf8_lossy(&out.stdout);
            // Simple TOML-like parser for depends
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with("depends") {
                    let inner = line
                        .strip_prefix("depends")
                        .and_then(|s| s.split('=').nth(1))
                        .map(|s| s.trim())
                        .unwrap_or("");
                    let inner = inner
                        .strip_prefix('[')
                        .and_then(|s| s.strip_suffix(']'))
                        .unwrap_or("");
                    if inner.is_empty() {
                        return Vec::new();
                    }
                    return inner
                        .split(',')
                        .map(|s| {
                            s.trim()
                                .trim_matches('"')
                                .trim()
                                .to_string()
                        })
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn clean_all() {
    let dirs = [
        PathBuf::from(BUILD_DIR),
        PathBuf::from(DESTDIR_BASE),
        PathBuf::from(iso::ISO_WORK_DIR),
    ];
    for d in &dirs {
        if d.exists() {
            fs::remove_dir_all(d).ok();
            println!("  cleaned: {}", d.display());
        }
    }
    println!("\x1b[1mBuild artifacts cleaned.\x1b[0m");
}

fn list_built() {
    let packages_dir = PathBuf::from(OUTPUT_DIR).join("packages");
    if !packages_dir.exists() {
        println!("No packages built yet.");
        return;
    }
    let entries = fs::read_dir(&packages_dir).unwrap();
    let mut count = 0;
    for entry in entries.flatten() {
        if entry.path().extension().map_or(false, |e| e == "swell") {
            println!("  {}", entry.file_name().to_string_lossy());
            count += 1;
        }
    }
    let total_size: u64 = fs::read_dir(&packages_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().map_or(false, |e| e == "swell"))
        .filter_map(|e| fs::metadata(e.path()).ok())
        .map(|m| m.len())
        .sum();
    println!(
        "\x1b[1mTotal:\x1b[0m {} packages ({:.1} MB)",
        count,
        total_size as f64 / 1048576.0
    );
}

fn info_package(name: &str) -> Result<(), String> {
    let packages_dir = PathBuf::from(OUTPUT_DIR).join("packages");
    if !packages_dir.exists() {
        return Err("no packages built yet".to_string());
    }

    let entries = fs::read_dir(&packages_dir)
        .map_err(|e| format!("read packages: {}", e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "swell") {
            let filename = path.file_name().unwrap().to_string_lossy().to_string();
            let stem = filename.strip_suffix(".swell").unwrap_or(&filename);
            let parts: Vec<&str> = stem.rsplitn(4, '-').collect();
            if parts.len() >= 4 && parts[3] == name {
                let metadata = fs::metadata(&path).map_err(|e| format!("metadata: {}", e))?;
                let size = metadata.len();
                let sha = sha256_file(&path);
                let depends = extract_depends_from_archive(&path);

                println!("\x1b[1mPackage:\x1b[0m    {}", parts[3]);
                println!("\x1b[1mVersion:\x1b[0m    {}-{}", parts[2], parts[1]);
                println!("\x1b[1mArch:\x1b[0m       {}", parts[0]);
                println!("\x1b[1mDepends:\x1b[0m    {}", if depends.is_empty() { "(none)".to_string() } else { depends.join(", ") });
                println!("\x1b[1mSize:\x1b[0m        {} KB", size / 1024);
                println!("\x1b[1mSHA256:\x1b[0m     {}", sha);
                println!("\x1b[1mArchive:\x1b[0m    {}", filename);
                return Ok(());
            }
        }
    }
    Err(format!("package not found in repo: {}", name))
}

fn collect_files(base: &Path, dir: &Path, files: &mut Vec<String>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_files(base, &path, files);
            } else if let Ok(relative) = path.strip_prefix(base) {
                files.push(format!("/{}", relative.display()));
            }
        }
    }
}

fn sha256_file(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    if let Ok(data) = fs::read(path) {
        let mut hasher = Sha256::new();
        hasher.update(&data);
        format!("{:x}", hasher.finalize())
    } else {
        String::new()
    }
}
