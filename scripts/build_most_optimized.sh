#!/usr/bin/env bash
RUSTFLAGS="-Cpanic=abort -Ccodegen-units=1 -Cembed-bitcode=yes -Zdylib-lto -Copt-level=s -Zlocation-detail=none -Cstrip=symbols -Ctarget-cpu=native" cargo -Z build-std=std,panic_abort -Z build-std-features=panic_immediate_abort build --profile production --target "$(rustc -vV | grep host | cut -d " " -f2)"
