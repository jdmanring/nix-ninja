use anyhow::{anyhow, Context, Result};
use clang_sys::*;
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

struct VisitorData<'a> {
    includes: &'a mut HashSet<PathBuf>,
}

pub fn retrieve_c_includes(cmdline: &str, files: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut all_includes = HashSet::new();

    // Build compiler arguments from cmdline once
    let args = build_clang_args(cmdline)?;

    // Initialize clang index with diagnostics suppressed and reuse it
    let index = unsafe { clang_createIndex(0, 0) };
    if index.is_null() {
        return Err(anyhow!("Failed to create clang index"));
    }

    // Try to parse all files in a single translation unit for better performance
    // A PARSE FAILURE IS AN ERROR, NOT AN EMPTY ANSWER. Skipping it returns
    // zero includes for the translation unit, the task derivation is built
    // without them, and the compile then fails inside the sandbox on a missing
    // header or, worse, succeeds against a different one. Over-declaring costs
    // an upload; under-declaring produces a wrong artifact quietly, so the two
    // directions are not symmetric and the caller has to be told.
    //
    // The named case, a generated header that does not exist yet, is real and
    // is the caller's to handle: it knows which paths the build declares and
    // has not yet written, and this module does not.
    let parse = |file: &PathBuf| -> Result<()> {
        let file_includes = parse_file_includes(index, file, &args)
            .with_context(|| format!("libclang could not parse {}", file.display()))?;
        all_includes.extend(file_includes);
        Ok(())
    };
    let result = files.iter().try_for_each(parse);
    if let Err(e) = result {
        unsafe { clang_disposeIndex(index) };
        return Err(e);
    }

    // Cleanup
    unsafe {
        clang_disposeIndex(index);
    }

    let mut result: Vec<PathBuf> = all_includes.into_iter().collect();
    result.sort();
    Ok(result)
}

fn build_clang_args(cmdline: &str) -> Result<Vec<CString>> {
    // Split the command line using POSIX shell semantics
    let mut cmdline_args =
        shell_words::split(cmdline).map_err(|e| anyhow!("Invalid command line syntax: {}", e))?;

    // Skip the compiler executable (first argument)
    if !cmdline_args.is_empty() {
        cmdline_args.remove(0);
    }

    // Filter out compilation-specific flags that libclang doesn't need
    let mut filtered_args = Vec::new();
    let mut i = 0;
    while i < cmdline_args.len() {
        let arg = &cmdline_args[i];

        // Skip source files (they'll be passed separately to clang_parseTranslationUnit)
        if arg.ends_with(".cpp")
            || arg.ends_with(".c")
            || arg.ends_with(".cc")
            || arg.ends_with(".cxx")
        {
            i += 1;
            continue;
        }

        // Keep all other arguments (include paths, defines, warnings, etc.)
        filtered_args.push(arg.clone());
        i += 1;
    }

    // Add flags to suppress system header diagnostics and use dependency mode
    filtered_args.push("-w".to_string()); // Suppress all warnings
    filtered_args.push("-Wno-error".to_string()); // Don't treat warnings as errors
    filtered_args.push("-fsyntax-only".to_string()); // Skip code generation

    // Convert to CStrings
    filtered_args
        .into_iter()
        .map(|arg| CString::new(arg).map_err(|e| anyhow!("Invalid argument: {}", e)))
        .collect()
}

fn parse_file_includes(
    index: CXIndex,
    file_path: &Path,
    args: &[CString],
) -> Result<HashSet<PathBuf>> {
    let mut includes = HashSet::new();

    // A source path that is not UTF-8 is a real input class, not an
    // impossibility: this crate has a test for one. Refusing by name beats
    // panicking inside a driver that is mid-graph.
    let file_str = file_path
        .to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", file_path.display()))?;
    let file_cstring = CString::new(file_str)?;

    // Convert args to pointers
    let arg_ptrs: Vec<*const c_char> = args.iter().map(|s| s.as_ptr()).collect();

    // Minimal parsing - just enough to get include information
    let tu = unsafe {
        clang_parseTranslationUnit(
            index,
            file_cstring.as_ptr(),
            arg_ptrs.as_ptr(),
            arg_ptrs.len() as i32,
            ptr::null_mut(),
            0,
            CXTranslationUnit_None, // Try with no special flags first
        )
    };

    if tu.is_null() {
        return Err(anyhow!("Failed to parse translation unit for {}", file_str));
    }

    // A NON-NULL TRANSLATION UNIT IS NOT A CLAIM THAT THE PARSE SUCCEEDED.
    // clang returns a usable unit for a source with fatal errors, and
    // clang_getInclusions then reports only the includes it resolved before
    // stopping. Returning that as the answer declares a fraction of the real
    // inputs and the sandbox compile reads whatever survives, which is the
    // under-declaration this crate exists to prevent. One unresolvable
    // header - a generated one, a missing -I, a header behind an undefined
    // -D - is enough to trigger it.
    let fatal = unsafe {
        let n = clang_getNumDiagnostics(tu);
        (0..n).any(|i| {
            let d = clang_getDiagnostic(tu, i);
            let severity = clang_getDiagnosticSeverity(d);
            clang_disposeDiagnostic(d);
            severity >= CXDiagnostic_Error
        })
    };
    if fatal {
        unsafe { clang_disposeTranslationUnit(tu) };
        return Err(anyhow!(
            "libclang reported errors parsing {file_str}; the include set \
             would be incomplete"
        ));
    }

    // Collect all inclusions using clang_getInclusions
    let mut visitor_data = VisitorData {
        includes: &mut includes,
    };
    unsafe {
        clang_getInclusions(
            tu,
            inclusion_visitor,
            &mut visitor_data as *mut VisitorData as *mut c_void,
        );
    }

    // Cleanup
    unsafe {
        clang_disposeTranslationUnit(tu);
    }

    Ok(includes)
}

extern "C" fn inclusion_visitor(
    file: CXFile,
    _inclusion_stack: *mut CXSourceLocation,
    _include_len: c_uint,
    client_data: CXClientData,
) {
    if file.is_null() {
        return;
    }

    let visitor_data = unsafe { &mut *(client_data as *mut VisitorData) };

    unsafe {
        // NO SYSTEM-HEADER FILTER. Under nix there is no system: every header
        // arrives as a store path reached through -I or -isystem, so a header
        // dropped for looking like a system header is a missing input, and a
        // missing input is the failure this crate exists to prevent.
        // clang_Location_isInSystemHeader reports true for anything reached
        // through -isystem, which is how a nix toolchain presents libstdc++.

        let file_name = clang_getFileName(file);
        let file_name_ptr = clang_getCString(file_name);
        if !file_name_ptr.is_null() {
            if let Ok(file_path_str) = CStr::from_ptr(file_name_ptr).to_str() {
                visitor_data.includes.insert(PathBuf::from(file_path_str));
            }
        }
        clang_disposeString(file_name);
    }
}
