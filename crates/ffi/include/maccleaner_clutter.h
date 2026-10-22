#ifndef MACCLEANER_CLUTTER_H
#define MACCLEANER_CLUTTER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* My Clutter and Space Lens. They return the usual JSON envelope; free with mcl_string_free.
   Blocking: call them off the main thread. Sizes are in bytes allocated on disk. */

/* {roots?:[string], min_bytes?:u64, older_than_days?:u32, limit?:u32, include_library?:bool}
   -> LargeFilesReport. Defaults: roots=[$HOME], min_bytes=50 MB, limit=500; excludes ~/Library,
   hidden folders and .app bundles. */
char *mcl_large_files(const char *request_json);
/* {roots?:[string], min_bytes?:u64, limit?:u32} -> DuplicatesReport (min_bytes=1 MB, limit=300). */
char *mcl_duplicates(const char *request_json);
/* -> SpaceNode. depth is clamped to 1..3; top to a minimum of 1. */
char *mcl_space_tree(const char *path, uint32_t depth, uint32_t top);
/* {paths:[string], dry_run?:bool} -> CleanReport; dry_run defaults to true. */
char *mcl_trash_paths(const char *request_json);

#ifdef __cplusplus
}
#endif

#endif
