#[macro_export]
macro_rules! cbsh {
    (cwd: $cwd:expr, $path:expr, $($part:expr),*) => {{
        use $crate::support::fs::DisplayPath;

        let path = format!($path, $(
            $part.display_path()
        ),*);

        cbsh!($cwd, &path)
    }};

    (cwd: $cwd:expr, $path:expr) => {{
        cbsh!($cwd, $path)
    }};

    ($cwd:expr, $path:expr) => {{
        pub use itertools::Itertools;
        pub use std::io::prelude::*;
        pub use std::process::{Command, Stdio};
        pub use crate::common::support::{NATIVE_PATH_ENV_VAR, LOGGER_PREFIX, fs, shell_os_paths, macros, Outcome};
        pub use std::time::Instant;
        pub use std::ops::Sub;
        use std::env;

        let test_bins = fs::binaries();

        let cwd = std::env::current_dir().expect("Could not get current working directory.");
        let test_bins = nu_path::canonicalize_with(&test_bins, cwd).unwrap_or_else(|e| {
            panic!(
                "Couldn't canonicalize dummy binaries path {}: {:?}",
                test_bins.display(),
                e
            )
        });

        let mut paths = shell_os_paths();
        paths.insert(0, test_bins);

        let path = $path.lines().collect::<Vec<_>>().join("; ");

        let paths_joined = match std::env::join_paths(paths) {
            Ok(all) => all,
            Err(_) => panic!("Couldn't join paths for PATH var."),
        };

        let target_cwd = fs::in_directory(&$cwd);

        let start = Instant::now();
        let process = match Command::new(fs::executable_path())
            .env("PWD", &target_cwd)  // setting PWD is enough to set cwd
            .env(NATIVE_PATH_ENV_VAR, paths_joined)
            .envs(env::vars())  // passthrough env vars
            .current_dir(&target_cwd)
            .arg("--logger-prefix")
            .arg(format!("\"{}\"", LOGGER_PREFIX.to_string()))
            .arg("-c")
            .arg(format!("{}", fs::DisplayPath::display_path(&path)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(why) => panic!("Can't run test {:?} {}", fs::executable_path(), why.to_string()),
        };

        // let stdin = process.stdin.as_mut().expect("couldn't open stdin");
        // stdin
        //     .write_all(commands.as_bytes())
        //     .expect("couldn't write to stdin");

        let output = process
            .wait_with_output()
            .expect("couldn't read from stdout/stderr");

        let out = macros::read_std(&output.stdout);
        let err = String::from_utf8_lossy(&output.stderr);
        let taken = Instant::now().sub(start);

        println!("\n=== cmd\n{:?}", path);
        println!("Took: {:?}\n", taken);
        println!("=== stdout\n{}", out);
        println!("=== stderr\n");

        let lines = err.split('\n');
        let mut actual_err = Vec::new();
        for line in lines {
            if line.starts_with(LOGGER_PREFIX) {
                println!("{}\n", line.strip_prefix(LOGGER_PREFIX).unwrap());
            } else {
                actual_err.push(line);
            }
        }

        Outcome::new(out, actual_err.join("\n"))
    }};
}

pub fn read_std(std: &[u8]) -> String {
    let out = String::from_utf8_lossy(std);
    let out = out.lines().collect::<Vec<_>>().join("\n");
    let out = out.replace("\r\n", "");
    out.replace('\n', "")
}
