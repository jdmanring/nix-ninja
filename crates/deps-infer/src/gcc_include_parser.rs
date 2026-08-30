use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// Parse include directories from a gcc cmdline.
pub fn parse_include_dirs(cmdline: &str) -> Result<Vec<PathBuf>> {
    // Split the command line respecting quotes and escapes
    let args = match shell_words::split(cmdline) {
        Ok(args) => args,
        Err(e) => return Err(anyhow!("Invalid command line syntax: {}", e)),
    };

    // THE ORDER IS THE CONTRACT, not just the set. This list is the
    // scanner's search order for resolving an include to ONE file, and
    // two gcc rules bit when it diverged from the compiler's own order
    // (gcc-16's build-libiberty, 2026-08-24): -idirafter directories are
    // searched AFTER every -I directory, not at their argv position; and
    // a -I directory that also appears as -isystem/-idirafter is IGNORED
    // at its -I position and searched at the system position instead
    // (gcc docs, "If a standard system include directory ... is also
    // specified with -I, the -I option is ignored"). nixpkgs' cc-wrapper
    // produces exactly that shape - the libc include dir as an early -I
    // and again as -idirafter - so without the demotion the scanner
    // resolved glibc's OLD obstack.h where gcc itself resolves the
    // project's, declared the wrong dependency, and the task compiled
    // against a header the real build never sees.
    let mut i_dirs = Vec::<PathBuf>::new(); // -iquote, -I, -isystem, in order
    let mut after_dirs = Vec::<PathBuf>::new(); // -idirafter, in order
    let mut system_dirs = Vec::<PathBuf>::new(); // -isystem + -idirafter (demotion set)
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        // Case 1: -Idir (no space)
        if arg.starts_with("-I") && arg.len() > 2 && !arg[2..].starts_with('=') {
            i_dirs.push(arg[2..].to_string().into());
        }
        // Case 2: -I dir (with space)
        else if arg == "-I" && i + 1 < args.len() {
            i_dirs.push(args[i + 1].to_string().into());
            i += 1; // Skip the next argument as we've consumed it
        }
        // Case 3: -I=dir (with equals sign)
        else if let Some(stripped) = arg.strip_prefix("-I=") {
            i_dirs.push(stripped.to_string().into());
        }
        // -iquote, -isystem, -idirafter: gcc's other include-dir flags,
        // attached or separate. -iquote is where skarnet builds put their
        // GENERATED headers (s6-linux-init: `-iquote src/include-local`
        // holding initctl.h and defaults.h), and a flag this parser does
        // not read is a directory the scanner cannot resolve through - the
        // header exists, gcc finds it, and the task never receives it
        // (eighth class, 2026-08-23).
        else if let Some(rest) = ["-iquote", "-isystem", "-idirafter"]
            .iter()
            .find_map(|f| arg.strip_prefix(f))
        {
            let dir: Option<PathBuf> = if rest.is_empty() {
                if i + 1 < args.len() {
                    i += 1;
                    Some(args[i].to_string().into())
                } else {
                    None
                }
            } else {
                Some(rest.to_string().into())
            };
            if let Some(d) = dir {
                if arg.starts_with("-idirafter") {
                    system_dirs.push(d.clone());
                    after_dirs.push(d);
                } else if arg.starts_with("-isystem") {
                    system_dirs.push(d.clone());
                    i_dirs.push(d);
                } else {
                    i_dirs.push(d);
                }
            }
        }

        i += 1;
    }

    // Demotion: a -I/-iquote entry that duplicates a system dir is
    // ignored at its early position (it will be reached via after_dirs).
    // Lexical comparison, which covers the cc-wrapper shape that bit -
    // both spellings are the identical store-path string. An -isystem
    // entry is its own system position and is NOT demoted by itself.
    let _ = &system_dirs; // -isystem entries keep their argv position
    let mut include_dirs: Vec<PathBuf> = Vec::new();
    for d in i_dirs {
        if after_dirs.contains(&d) {
            continue; // demoted: searched at its -idirafter position below
        }
        include_dirs.push(d);
    }
    include_dirs.extend(after_dirs);

    Ok(include_dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper function to convert string slices to PathBufs
    fn paths(dirs: &[&str]) -> Vec<PathBuf> {
        dirs.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn iquote_isystem_idirafter_are_include_dirs() {
        // s6-linux-init's real shape: generated headers reachable only
        // through -iquote. Attached and separate forms both count.
        assert_eq!(
            parse_include_dirs("gcc -iquote src/include-local -Isrc/include -c x.c").unwrap(),
            paths(&["src/include-local", "src/include"])
        );
        assert_eq!(
            parse_include_dirs("gcc -isystem/opt/inc -idirafter late -c x.c").unwrap(),
            paths(&["/opt/inc", "late"])
        );
    }

    #[test]
    fn idirafter_sorts_last_and_demotes_its_early_i_twin() {
        // nixpkgs cc-wrapper's real shape on gcc's own build: the libc
        // include dir appears BOTH as an early -I and as -idirafter. gcc
        // ignores the -I occurrence and searches the dir last; a scanner
        // that honors the argv position resolves the libc's obstack.h
        // where gcc resolves the project's (gcc-16 build-libiberty,
        // 2026-08-24). The parse must put the twin at the END only.
        assert_eq!(
            parse_include_dirs(
                "gcc -c -I/libc/include -B/libc/lib -idirafter /libc/include -I. -I../include x.c"
            )
            .unwrap(),
            paths(&[".", "../include", "/libc/include"])
        );
        // A plain -idirafter with no twin still sorts after every -I.
        assert_eq!(
            parse_include_dirs("gcc -idirafter late -Iearly -c x.c").unwrap(),
            paths(&["early", "late"])
        );
    }

    #[test]
    fn test_basic_cases() {
        assert_eq!(
            parse_include_dirs("g++ -Idir1 file.cpp").unwrap(),
            paths(&["dir1"])
        );
        assert_eq!(
            parse_include_dirs("g++ -I dir2 file.cpp").unwrap(),
            paths(&["dir2"])
        );
        assert_eq!(
            parse_include_dirs("g++ -I=dir3 file.cpp").unwrap(),
            paths(&["dir3"])
        );
    }

    #[test]
    fn test_multiple_includes() {
        assert_eq!(
            parse_include_dirs("g++ -Idir1 -Idir2 -I dir3 file.cpp").unwrap(),
            paths(&["dir1", "dir2", "dir3"])
        );
    }

    #[test]
    fn test_paths_with_spaces() {
        assert_eq!(
            parse_include_dirs("g++ -I\"dir with spaces\" file.cpp").unwrap(),
            paths(&["dir with spaces"])
        );
        assert_eq!(
            parse_include_dirs("g++ -I 'dir with spaces' file.cpp").unwrap(),
            paths(&["dir with spaces"])
        );
        assert_eq!(
            parse_include_dirs("g++ -I=dir\\ with\\ spaces file.cpp").unwrap(),
            paths(&["dir with spaces"])
        );
    }

    #[test]
    fn test_multiple_spaces() {
        assert_eq!(
            parse_include_dirs("g++ -I   dir4 file.cpp").unwrap(),
            paths(&["dir4"])
        );
    }

    #[test]
    fn test_mixed_with_other_options() {
        assert_eq!(
            parse_include_dirs("g++ -Wall -Wextra -O2 -Idir1 -I dir2 -I=dir3 -c file.cpp").unwrap(),
            paths(&["dir1", "dir2", "dir3"])
        );
    }

    #[test]
    fn test_absolute_paths() {
        assert_eq!(
            parse_include_dirs("g++ -I/usr/include -I /opt/include file.cpp").unwrap(),
            paths(&["/usr/include", "/opt/include"])
        );
    }

    #[test]
    fn test_relative_paths() {
        assert_eq!(
            parse_include_dirs("g++ -I../include -I ./local/include file.cpp").unwrap(),
            paths(&["../include", "./local/include"])
        );
    }

    #[test]
    fn test_paths_with_special_chars() {
        assert_eq!(
            parse_include_dirs("g++ -I/path/to/my-includes -I=/path/to/your_includes file.cpp")
                .unwrap(),
            paths(&["/path/to/my-includes", "/path/to/your_includes"])
        );
    }

    #[test]
    fn test_invalid_syntax() {
        // Test with unmatched quotes
        assert!(parse_include_dirs("g++ -I\"unclosed quote file.cpp").is_err());
    }
}
