#!/usr/bin/env bash
# 1. Pull the git source of poppler
# 2. cd poppler
# 3. git checkout poppler-23.07.0
# 4. mkdir build
# 5. cd build
# 6. cmake .. -DENABLE_GPGME=OFF -DENABLE_QT5=OFF -DENABLE_QT6=OFF -DENABLE_BOOST=OFF -DBUILD_SHARED_LIBS=OFF
# 7. cmake --build . --parallel $(nproc)
env SYSTEM_DEPS_POPPLER_GLIB_LINK=static \
	SYSTEM_DEPS_POPPLER_GLIB_NO_PKG_CONFIG=1 \
	SYSTEM_DEPS_POPPLER_GLIB_SEARCH_NATIVE=/path/to/poppler/build/glib \
	SYSTEM_DEPS_POPPLER_GLIB_LIB=poppler-glib \
	cargo perf --bin for_profiling --
