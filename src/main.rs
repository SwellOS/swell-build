use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const PACKAGES_REPO: &str = "/usr/src/swell/packages";
const OUTPUT_DIR: &str = "/usr/src/swell/repo";
const BUILD_DIR: &str = "/usr/src/swell/build";
const SOURCE_DIR: &str = "/usr/src/swell/sources";
const DESTDIR_BASE: &str = "/usr/src/swell/dest";

#[derive(Parser)]
#[command(name = "swell-build", about = "SwellOS build system")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build a package from source
    Pkg {
        name: String,
    },
    /// Build all packages
    World,
    /// Generate repo.db from built packages
    Repo,
    /// Clean build artifacts
    Clean,
    /// List built packages
    List,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Pkg { name } => {
            if let Err(e) = build_package(&name) {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::World => {
            if let Err(e) = build_all() {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Repo => {
            if let Err(e) = generate_repo_db() {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Clean => {
            clean_all();
        }
        Commands::List => {
            list_built();
        }
    }
}

fn build_package(name: &str) -> Result<(), String> {
    // Find package in all categories
    let categories = ["core", "desktop", "browser", "dev", "gaming", "multimedia", "office", "network"];
    let mut pkg_path = None;

    for cat in &categories {
        let candidate = Path::new(PACKAGES_REPO).join(cat).join(name);
        if candidate.is_dir() {
            pkg_path = Some(candidate);
            break;
        }
    }

    let pkg_path = pkg_path.ok_or_else(|| format!("package not found: {}", name))?;

    let version = fs::read_to_string(pkg_path.join("version"))
        .map_err(|_| format!("no version file for {}", name))?
        .trim()
        .to_string();

    let sources = if pkg_path.join("sources").exists() {
        fs::read_to_string(pkg_path.join("sources"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let swellbuild = pkg_path.join("swellbuild");
    if !swellbuild.exists() {
        return Err(format!("no swellbuild script for {}", name));
    }

    let _patches_dir = pkg_path.join("patches");

    println!("Building {} {}...", name, version);

    // Setup directories
    let src_dir = PathBuf::from(SOURCE_DIR).join(name);
    let build_dir = PathBuf::from(BUILD_DIR).join(name);
    let dest_dir = PathBuf::from(DESTDIR_BASE).join(name);

    if build_dir.exists() {
        fs::remove_dir_all(&build_dir).map_err(|e| format!("clean build dir: {}", e))?;
    }
    if dest_dir.exists() {
        fs::remove_dir_all(&dest_dir).map_err(|e| format!("clean dest dir: {}", e))?;
    }

    fs::create_dir_all(&src_dir).map_err(|e| format!("source dir: {}", e))?;
    fs::create_dir_all(&build_dir).map_err(|e| format!("build dir: {}", e))?;
    fs::create_dir_all(&dest_dir).map_err(|e| format!("dest dir: {}", e))?;

    // Download sources
    for url in &sources {
        let filename = url.split('/').last().unwrap_or("unknown");
        let dest = src_dir.join(filename);
        if !dest.exists() {
            println!("  Fetching: {}", url);
            let status = Command::new("curl")
                .args(["-L", "-o", &dest.to_string_lossy(), url])
                .status()
                .map_err(|e| format!("curl failed: {}", e))?;
            if !status.success() {
                return Err(format!("failed to download {}", url));
            }
        }
    }

    // Run build script
    let sbpm_lib = format!(
        r#"
DESTDIR="{dd}"
SRCDIR="{sd}"

fetch_url() {{
    echo "fetch_url: $1"
}}

unpack() {{
    local archive="$1"
    case "$archive" in
        *.tar.gz|*.tgz) tar xzf "{sd}/$archive" ;;
        *.tar.xz)       tar xJf "{sd}/$archive" ;;
        *.tar.bz2)      tar xjf "{sd}/$archive" ;;
        *.tar)          tar xf "{sd}/$archive" ;;
        *.zip)          unzip "{sd}/$archive" ;;
        *)              echo "Unknown archive: $archive"; return 1 ;;
    esac
}}
"#,
        dd = dest_dir.display(),
        sd = src_dir.display(),
    );

    let script = format!(
        r#"set -e
{}
source "{}"

if declare -f pkg_fetch > /dev/null; then
    cd "{}"
    pkg_fetch
fi

if declare -f pkg_unpack > /dev/null; then
    cd "{}"
    pkg_unpack
fi

cd_dir() {{
    for d in "${{srcdir:-.}}"/*/; do
        if [ -d "$d" ]; then
            cd "$d"
            return 0
        fi
    done
    return 1
}}

srcdir="{}"
for d in "$srcdir"*/; do
    if [ -d "$d" ]; then
        cd "$d"
        break
    fi
done

if declare -f pkg_build > /dev/null; then
    pkg_build
fi

if declare -f pkg_install > /dev/null; then
    pkg_install
fi
"#,
        sbpm_lib,
        swellbuild.display(),
        src_dir.display(),
        src_dir.display(),
        src_dir.display(),
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
        return Err(format!("build failed:\nSTDOUT:\n{}\nSTDERR:\n{}", stdout, stderr));
    }

    // Collect installed files
    let mut installed_files = Vec::new();
    collect_files(&dest_dir, &dest_dir, &mut installed_files);

    // Create .swell archive
    let output_dir = PathBuf::from(OUTPUT_DIR).join("packages");
    fs::create_dir_all(&output_dir).map_err(|e| format!("output dir: {}", e))?;

    let archive_name = format!("{}-{}-1-x86_64.swell", name, version);
    let archive_path = output_dir.join(&archive_name);

    // Write metadata
    let metadata = format!(
        r#"name = "{}"
version = "{}"
release = 1
arch = "x86_64"
"#,
        name, version
    );
    fs::write(dest_dir.join("metadata.toml"), &metadata)
        .map_err(|e| format!("metadata: {}", e))?;

    // Write manifest
    let manifest_content = installed_files.join("\n");
    fs::write(dest_dir.join("manifest"), &manifest_content)
        .map_err(|e| format!("manifest: {}", e))?;

    // Create .swell archive (tar.zst)
    println!("  Creating archive...");
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
        // Try without root/ (some packages may not have files in root)
        let status = Command::new("tar")
            .args([
                "--zstd", "-cf",
                &archive_path.to_string_lossy(),
                "-C", &dest_dir.to_string_lossy(),
                "metadata.toml", "manifest",
            ])
            .status()
            .map_err(|e| format!("tar failed: {}", e))?;
        if !status.success() {
            return Err("failed to create archive".to_string());
        }
    }

    println!("  Done: {}/{}", output_dir.display(), archive_name);
    Ok(())
}

fn build_all() -> Result<(), String> {
    let categories = ["core", "desktop", "browser", "dev", "gaming", "multimedia", "office", "network"];
    for cat in &categories {
        let cat_path = Path::new(PACKAGES_REPO).join(cat);
        if !cat_path.is_dir() {
            continue;
        }
        let entries = fs::read_dir(&cat_path).map_err(|e| format!("read dir: {}", e))?;
        for entry in entries.flatten() {
            let pkg_path = entry.path();
            if pkg_path.is_dir() {
                if let Some(name) = pkg_path.file_name().and_then(|n| n.to_str()) {
                    // Skip metapackages (they don't have build scripts)
                    if pkg_path.join("swellbuild").exists() {
                        println!("\n=== Building {} ===", name);
                        if let Err(e) = build_package(name) {
                            eprintln!("  ERROR: {}", e);
                        }
                    }
                }
            }
        }
    }
    println!("\nAll packages built.");
    Ok(())
}

fn generate_repo_db() -> Result<(), String> {
    let packages_dir = PathBuf::from(OUTPUT_DIR).join("packages");
    if !packages_dir.exists() {
        return Err("no packages built yet".to_string());
    }

    let mut db: HashMap<String, serde_json::Value> = HashMap::new();

    let entries = fs::read_dir(&packages_dir).map_err(|e| format!("read packages: {}", e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "swell") {
            let filename = path.file_name().unwrap().to_string_lossy().to_string();
            let metadata = fs::metadata(&path).map_err(|e| format!("metadata: {}", e))?;
            let size = metadata.len();
            let sha = sha256_file(&path);

            // Parse name-version-release-arch.swell
            let parts: Vec<&str> = filename.rsplitn(4, '-').collect();
            if parts.len() >= 4 {
                let arch = parts[0].replace(".swell", "");
                let release = parts[1];
                let version = parts[2];
                let name = parts[3];

                let entry = serde_json::json!({
                    "version": version,
                    "release": release.parse::<u32>().unwrap_or(1),
                    "arch": arch,
                    "depends": [],
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

    println!("Generated: {} ({} packages)", repo_path.display(), db.len());
    Ok(())
}

fn clean_all() {
    let dirs = [
        PathBuf::from(BUILD_DIR),
        PathBuf::from(DESTDIR_BASE),
    ];
    for d in &dirs {
        if d.exists() {
            fs::remove_dir_all(d).ok();
            println!("Cleaned: {}", d.display());
        }
    }
    println!("Build artifacts cleaned.");
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
    println!("Total: {} packages", count);
}

fn collect_files(base: &Path, dir: &Path, files: &mut Vec<String>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_files(base, &path, files);
            } else {
                if let Ok(relative) = path.strip_prefix(base) {
                    files.push(format!("/{}", relative.display()));
                }
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
