use anyhow::{Context, Result};
use clap::Parser;
use flate2::read::GzDecoder;
use maturin::{BuildOptions, CargoOptions};
use pretty_assertions::assert_eq;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::iter::FromIterator;
use std::path::{Path, PathBuf};
use tar::Archive;
use zip::ZipArchive;

/// Tries to compile a sample crate (pyo3-pure) for musl,
/// given that rustup and the the musl target are installed
///
/// The bool in the Ok() response says whether the test was actually run
pub fn test_musl() -> Result<bool> {
    use anyhow::bail;
    use fs_err as fs;
    use fs_err::File;
    use goblin::elf::Elf;
    use std::io::ErrorKind;
    use std::process::Command;

    let get_target_list = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output();

    match get_target_list {
        Ok(output) => {
            if output.status.success() {
                let has_musl = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line.trim() == "x86_64-unknown-linux-musl");
                if !has_musl {
                    return Ok(false);
                }
            } else {
                bail!(
                    "`rustup target list --installed` failed with status {}",
                    output.status
                )
            }
        }
        // Ignore installations without rustup
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    };

    // The first arg gets ignored
    let options: BuildOptions = BuildOptions::try_parse_from([
        "build",
        "--manifest-path",
        "test-crates/hello-world/Cargo.toml",
        "--interpreter",
        "python3",
        "--target",
        "x86_64-unknown-linux-musl",
        "--compatibility",
        "linux",
        "--quiet",
        "--target-dir",
        "test-crates/targets/test_musl",
        "--out",
        "test-crates/wheels/test_musl",
    ])?;

    let build_context = options.into_build_context(false, cfg!(feature = "faster-tests"), false)?;
    let built_lib =
        PathBuf::from("test-crates/targets/test_musl/x86_64-unknown-linux-musl/debug/hello-world");
    if built_lib.is_file() {
        fs::remove_file(&built_lib)?;
    }
    let wheels = build_context.build_wheels()?;
    assert_eq!(wheels.len(), 1);

    // Ensure that we've actually built for musl
    assert!(built_lib.is_file());
    let mut file = File::open(built_lib)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    let elf = Elf::parse(&buffer)?;
    assert_eq!(elf.libraries, Vec::<&str>::new());

    Ok(true)
}

/// Test that we ignore non-existent Cargo.lock file listed by `cargo package --list`,
/// which seems to only occur with workspaces.
/// See https://github.com/rust-lang/cargo/issues/7938#issuecomment-593280660 and
/// https://github.com/PyO3/maturin/issues/449
pub fn test_workspace_cargo_lock() -> Result<()> {
    // The first arg gets ignored
    let options: BuildOptions = BuildOptions::try_parse_from([
        "build",
        "--manifest-path",
        "test-crates/workspace/py/Cargo.toml",
        "--compatibility",
        "linux",
        "--quiet",
        "--target-dir",
        "test-crates/targets/test_workspace_cargo_lock",
        "--out",
        "test-crates/wheels/test_workspace_cargo_lock",
    ])?;

    let build_context = options.into_build_context(false, false, false)?;
    let source_distribution = build_context.build_source_distribution()?;
    assert!(source_distribution.is_some());

    Ok(())
}

pub fn test_source_distribution(
    package: impl AsRef<Path>,
    expected_files: Vec<&str>,
    expected_cargo_toml: Option<(&Path, &str)>,
    unique_name: &str,
) -> Result<()> {
    let manifest_path = package.as_ref().join("Cargo.toml");
    let sdist_directory = Path::new("test-crates").join("wheels").join(unique_name);

    let build_options = BuildOptions {
        out: Some(sdist_directory),
        cargo: CargoOptions {
            manifest_path: Some(manifest_path),
            quiet: true,
            target_dir: Some(PathBuf::from(
                "test-crates/targets/test_workspace_cargo_lock",
            )),
            ..Default::default()
        },
        ..Default::default()
    };

    let build_context = build_options.into_build_context(false, false, false)?;
    let (path, _) = build_context
        .build_source_distribution()?
        .context("Failed to build source distribution")?;

    let tar_gz = fs_err::File::open(path)?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    let mut files = BTreeSet::new();
    let mut file_count = 0;
    let mut cargo_toml = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        files.insert(format!("{}", entry.path()?.display()));
        file_count += 1;
        if let Some(cargo_toml_path) = expected_cargo_toml.as_ref().map(|(p, _)| *p) {
            if entry.path()? == cargo_toml_path {
                let mut contents = String::new();
                entry.read_to_string(&mut contents)?;
                cargo_toml = Some(contents);
            }
        }
    }
    assert_eq!(
        files,
        BTreeSet::from_iter(expected_files.into_iter().map(ToString::to_string))
    );
    assert_eq!(file_count, files.len(), "duplicated files found in sdist");

    if let Some((cargo_toml_path, expected)) = expected_cargo_toml {
        let cargo_toml = cargo_toml
            .with_context(|| format!("{} not found in sdist", cargo_toml_path.display()))?;
        assert_eq!(cargo_toml, expected);
    }
    Ok(())
}

pub fn check_wheel_files(
    package: impl AsRef<Path>,
    expected_files: Vec<&str>,
    unique_name: &str,
) -> Result<()> {
    let manifest_path = package.as_ref().join("Cargo.toml");
    let wheel_directory = Path::new("test-crates").join("wheels").join(unique_name);

    let build_options = BuildOptions {
        out: Some(wheel_directory),
        cargo: CargoOptions {
            manifest_path: Some(manifest_path),
            quiet: true,
            target_dir: Some(PathBuf::from(format!(
                "test-crates/targets/{}",
                unique_name
            ))),
            ..Default::default()
        },
        ..Default::default()
    };

    let build_context = build_options.into_build_context(false, false, false)?;
    let wheels = build_context
        .build_wheels()
        .context("Failed to build wheels")?;
    assert!(!wheels.is_empty());
    let (wheel_path, _) = &wheels[0];

    let wheel = ZipArchive::new(File::open(wheel_path)?)?;
    let drop_platform_specific_files = |file: &&str| -> bool {
        !matches!(Path::new(file).extension(), Some(ext) if ext == "pyc" || ext == "pyd" || ext == "so")
    };
    assert_eq!(
        wheel
            .file_names()
            .filter(drop_platform_specific_files)
            .collect::<BTreeSet<_>>(),
        expected_files.into_iter().collect::<BTreeSet<_>>()
    );
    Ok(())
}

pub fn abi3_python_interpreter_args() -> Result<()> {
    // Case 1: maturin build without `-i`, should work
    let options = BuildOptions::try_parse_from(vec![
        "build",
        "--manifest-path",
        "test-crates/pyo3-pure/Cargo.toml",
        "--quiet",
    ])?;
    let result = options.into_build_context(false, cfg!(feature = "faster-tests"), false);
    assert!(result.is_ok());

    // Case 2: maturin build -i python3.10, should work because python3.10 is in bundled sysconfigs
    let options = BuildOptions::try_parse_from(vec![
        "build",
        "--manifest-path",
        "test-crates/pyo3-pure/Cargo.toml",
        "--quiet",
        "-i",
        "python3.10",
    ])?;
    let result = options.into_build_context(false, cfg!(feature = "faster-tests"), false);
    assert!(result.is_ok());

    // Windows is a bit different so we exclude it from case 3 & 4

    // Case 3: maturin build -i python2.7, errors because python2.7 is supported
    #[cfg(not(windows))]
    {
        let options = BuildOptions::try_parse_from(vec![
            "build",
            "--manifest-path",
            "test-crates/pyo3-pure/Cargo.toml",
            "--quiet",
            "-i",
            "python2.7",
        ])?;
        let result = options.into_build_context(false, cfg!(feature = "faster-tests"), false);
        assert!(result.is_err());

        // Case 4: maturin build -i python-does-not-exists, errors because python executable is not found
        let options = BuildOptions::try_parse_from(vec![
            "build",
            "--manifest-path",
            "test-crates/pyo3-pure/Cargo.toml",
            "--quiet",
            "-i",
            "python-does-not-exists",
        ])?;
        let result = options.into_build_context(false, cfg!(feature = "faster-tests"), false);
        assert!(result.is_err());
    }

    Ok(())
}
