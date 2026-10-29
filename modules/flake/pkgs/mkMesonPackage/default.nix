{ lib
, coreutils
, findutils
, meson
, nix
, nix-ninja
, nix-ninja-task
, patchelf
, stdenv
}:

{ name ? "${args'.pname}-${args'.version}"
, src
, target
, nativeBuildInputs ? [ ]
, ...
}@args':

let
  normalizedTarget = builtins.replaceStrings ["/"] ["-"] target;

  # UPSTREAM #16: configure in its own derivation, so `meson setup` is
  # cached across builds instead of re-run inside every ninja derivation.
  #
  # OPT-IN, and deliberately so. Every derivation this driver emits is
  # keyed on what the driver emits, and a consumer can bank hundreds of thousands
  # of per-TU outputs against those keys. Making this the default flips
  # the configure phase for every consumer at once, on a change no package
  # has been driven through yet - so it ships switchable, gets verified on
  # a package, and the default moves after that rather than before it.
  # Passing nothing leaves `args'` without the attribute and the emitted
  # derivation byte-identical, which is what makes landing it free.
  useConfigureCache = args'.useConfigureCache or false;
  baseArgs = builtins.removeAttrs args' [ "useConfigureCache" "configureSourceFilter" ];

  # Narrow `src` to what `meson setup` reads, so a source edit leaves the
  # cached configure derivation alone.
  #
  # Dropping files by NAME is unsound: meson.build enumerates its sources,
  # and setup fails on the missing file (`ERROR: File main.cpp does not
  # exist` on the first attempt at this). So the narrowed tree keeps every
  # NAME and stubs the contents: build-system files verbatim, everything
  # else an empty file carrying the original's mode. Existence checks,
  # globs and source lists all pass; a content edit to a stubbed file
  # changes nothing in this tree.
  #
  # The stub tree is its own CONTENT-ADDRESSED derivation, which is what
  # makes the edit free: the output path hashes outputs, so a stubbed edit
  # rebuilds this cheap file walk and leaves the configure derivation - and
  # the `meson setup` it caches - untouched. An input-addressed copy would
  # re-key on every edit and buy nothing.
  #
  # Known limit: setup never COMPILES sources, but it does READ some - a
  # version taken from a file's contents, a configure_file over a source,
  # a script setup executes. Those packages keep the full `src` through
  # `configureSourceFilter`, which overrides this wholesale.
  configureSrc =
    if args' ? configureSourceFilter then
      args'.configureSourceFilter src
    else
      stdenv.mkDerivation {
        name = "${name}-configure-src";
        inherit src;
        __contentAddressed = true;
        nativeBuildInputs = [ coreutils findutils ];
        phases = [ "unpackPhase" "patchPhase" "installPhase" ];
        installPhase = ''
          runHook preInstall
          mkdir -p "$out"
          cd "$NIX_BUILD_TOP/$sourceRoot"
          find . -mindepth 1 -print0 | while IFS= read -r -d "" entry; do
            rel="''${entry#./}"
            if [ -L "$entry" ]; then
              mkdir -p "$out/$(dirname "$rel")"
              cp -P "$entry" "$out/$rel"
            elif [ -d "$entry" ]; then
              mkdir -p "$out/$rel"
            else
              mkdir -p "$out/$(dirname "$rel")"
              case "$(basename "$entry")" in
                meson.build|meson_options.txt|CMakeLists.txt|configure.ac|Makefile.am|*.cmake|*.in)
                  cp "$entry" "$out/$rel" ;;
                *)
                  touch -d "@1" "$out/$rel"
                  chmod --reference="$entry" "$out/$rel" ;;
              esac
            fi
          done
          runHook postInstall
        '';
      };

  # An ORDINARY derivation: a real $out, no builder-rpc-v0, no dynamic
  # derivations. That is the entire point - it is cacheable and
  # substitutable by any machine, which the ninja derivation is not.
  # THREE ATTRIBUTES A PACKAGE CAN SET THAT THIS OPTION CANNOT HONOUR.
  # Each would diverge silently, so each is refused with its reason.
  cacheBlockers =
    (lib.optional (args' ? configurePhase)
      "sets its own configurePhase, which the cached path replaces")
    ++ (lib.optional (args' ? dontUseMesonConfigure)
      "sets dontUseMesonConfigure, so the configure derivation would produce no build directory")
    ++ (lib.optional (args' ? mesonBuildDir && lib.hasPrefix "/" (toString args'.mesonBuildDir))
      "sets an absolute mesonBuildDir, whose relative spellings would not resolve after the copy");

  configureDrv = assert lib.assertMsg (cacheBlockers == [ ])
    "mkMesonPackage: useConfigureCache cannot be used with a package that ${lib.head cacheBlockers}";
    stdenv.mkDerivation (baseArgs // {
    name = "${name}-configure";

    # Existence and contents, per the comment on `configureSrc` above.
    src = configureSrc;

    # THE PREFIX MUST MATCH THE UNCACHED BUILD OR THE OPTION IS NOT
    # TRANSPARENT. stdenv defaults `prefix` to `$out`, and meson's setup hook
    # passes `--prefix=$prefix`, so without this the configure derivation
    # bakes a real store path into build.ninja and meson-private/coredata.dat
    # where the ordinary path bakes `/nonexistent` (the ninja derivation sets
    # `out = "/nonexistent"` for the builder-rpc reason above). Any package
    # deriving a compile flag from `get_option('prefix')` - a -DPREFIX=, an
    # install_rpath, a configure_file into a header - would then emit command
    # lines naming the configure output, which is an input of the ninja
    # derivation but NOT of the per-TU derivations it emits.
    # example-hello uses no prefix, which is exactly why it could not catch
    # this.
    prefix = "/nonexistent";

    # nix-ninja IS A CONFIGURE-TIME DEPENDENCY, which is not obvious and
    # is the first thing this derivation taught us. meson refuses to
    # configure without a ninja it can version-probe:
    #     ERROR: Could not detect Ninja v1.8.2 or newer
    # and the binary that answers that probe is the driver, which reports
    # 1.8.2 for exactly this reason. So splitting configure into its own
    # derivation does NOT decouple it from the driver: nix-ninja's store
    # path is an input of the configure derivation too, and meson records
    # the ninja command in build.ninja's regeneration rule. That bounds
    # what the cache buys - it survives source edits, not a driver bump -
    # and it is worth saying to the maintainer, because the issue's framing
    # ("cache the configure") reads as though it would survive both.
    nativeBuildInputs = [ coreutils meson nix-ninja ] ++ nativeBuildInputs;

    preConfigure = ''
      export NINJA="${nix-ninja}/bin/nix-ninja"
    '';

    # Configure, and stop. The build phase is where nix-ninja would run.
    buildPhase = ''
      runHook preBuild
      runHook postBuild
    '';

    # mesonConfigurePhase leaves the configured tree in $mesonBuildDir and
    # cds into it, so $out is that directory verbatim. Copied rather than
    # moved: meson's own files stay where build.ninja expects them.
    installPhase = ''
      runHook preInstall
      cd "$NIX_BUILD_TOP/$sourceRoot"
      cp -r "''${mesonBuildDir:-build}" "$out"
      runHook postInstall
    '';

    dontUseMesonInstall = true;
    dontUseMesonCheck = true;
    doCheck = false;
    doInstallCheck = false;
  });

  ninjaDrv = stdenv.mkDerivation (baseArgs // {
    name = "${name}.drv";

    # Unfortunately stdenv's `genericBuild` assumes the `out` variable is set.
    # That is generally a reasonable assumption as it is handled by nix,
    # but it is intentionally left unset when running with `builder-rpc-v0`.
    # For basic programs it is possible to avoid this hack by running `builtins.derivation`
    # directly, without nixpkgs.
    # For more complex programs, however, stdenv is necessary to run hooks, such as from `pkg-config`.
    out = "/nonexistent";

    nativeBuildInputs = [
      coreutils
      meson
      nix
      nix-ninja
      nix-ninja-task
      patchelf
    ] ++ nativeBuildInputs;

    requiredSystemFeatures = [ "builder-rpc-v0" ];

    preConfigure = ''
      export NIX_NINJA_DRV="true"
      export NINJA="${nix-ninja}/bin/nix-ninja"
      export NIX_CONFIG="extra-experimental-features = nix-command ca-derivations dynamic-derivations"
    '';

  } // lib.optionalAttrs useConfigureCache {
    # Replace `meson setup` with the cached tree. The spellings inside
    # build.ninja are relative (`../src/...`, see docs/design.md), and the
    # source root is /build/source in both derivations, so the configured
    # tree resolves identically here. Writable because the build writes
    # into it, and a store path is read-only.
    configurePhase = ''
      runHook preConfigure
      cp -r "${configureDrv}" "''${mesonBuildDir:-build}"
      chmod -R u+w "''${mesonBuildDir:-build}"
      cd "''${mesonBuildDir:-build}"
      runHook postConfigure
    '';
  } // {

    buildPhase = ''
      runHook preBuild
      nix-ninja ${target}
      runHook postBuild
    '';

    dontUseMesonInstall = true;
    dontUseMesonCheck = true;

    # stdenv adds a -rpath with a self reference but self references are not
    # allowed by text output.
    NIX_NO_SELF_RPATH = true;

    __contentAddressed = true;
    outputHashMode = "text";
    outputHashAlgo = "sha256";

    passthru = {
      target = builtins.outputOf ninjaDrv.outPath normalizedTarget;
    };
  });

in ninjaDrv
