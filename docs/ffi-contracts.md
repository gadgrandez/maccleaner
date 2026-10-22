# C API contracts (new modules)

Every function returns a `char *` with the JSON envelope `{"ok":bool,"data":...,"error":string|null}`
(use `respond` and `arg` from `crates/ffi/src/lib.rs`) and is freed with `mcl_string_free`.
Keys are `snake_case`; enums are `snake_case` strings. Sizes are in bytes allocated on disk
(`fsutil::usage`/`fsutil::usages`). Dates are Unix seconds (`u64`).
JSON requests with `dry_run` default it to `true`.
Destructive actions move items to the Trash (`trash::move_to_trash`) and return
`clean::CleanReport` (reuse the type; `category` may carry a free-form module id).

## Applications (`apps`)

```c
char *mcl_apps_list(void);                         /* -> [AppInfo] sorted by name */
char *mcl_apps_leftovers(const char *app_path);    /* -> AppLeftovers */
char *mcl_apps_orphans(void);                      /* -> [OrphanGroup] */
char *mcl_apps_uninstall(const char *request_json);/* {app_path?:string, leftover_paths:[string], dry_run?:bool} -> CleanReport */
char *mcl_extensions_list(void);                   /* -> [ExtensionItem] */
char *mcl_extensions_remove(const char *request_json); /* {paths:[string], dry_run?:bool} -> CleanReport */
```

```text
AppInfo { name, bundle_id?: string, version?: string, path, bytes: u64, is_apple: bool,
          source: "app_store"|"setapp"|"other", sparkle_feed_url?: string,
          archs: [string]   // e.g. ["arm64","x86_64"] of the main executable
        }
LeftoverItem { path, kind: "cache"|"preferences"|"app_support"|"container"|"group_container"|
               "saved_state"|"logs"|"launch_agent"|"cookies"|"webkit"|"http_storage"|"other", bytes: u64 }
AppLeftovers { app: AppInfo, items: [LeftoverItem], total_bytes: u64 }
OrphanGroup { id: string /* bundle id or name */, items: [LeftoverItem], total_bytes: u64 }
ExtensionItem { name, path, kind: "preference_pane"|"quicklook"|"spotlight"|"internet_plugin"|
                "screen_saver"|"services"|"mail_bundle", scope: "user"|"system", bytes: u64, removable: bool }
```

## Performance (`performance`)

```c
char *mcl_launch_items(void);                                  /* -> [LaunchItem] */
char *mcl_launch_item_set_enabled(const char *label, int enabled); /* -> LaunchItem */
char *mcl_launch_item_remove(const char *plist_path, int dry_run); /* -> CleanReport */
char *mcl_maintenance_tasks(void);                             /* -> [MaintenanceTask] */
char *mcl_maintenance_run(const char *ids_json);               /* ["flush_dns",...] -> [TaskResult] */
char *mcl_top_processes(uint32_t limit);                       /* -> [ProcessInfo] by memory desc. */
```

```text
LaunchItem { label, plist_path, program?: string, scope: "user_agent"|"global_agent"|"global_daemon",
             run_at_load: bool, keep_alive: bool, enabled: bool, loaded: bool, pid?: i32,
             app_name?: string /* containing app inferred from program */, removable: bool }
MaintenanceTask { id, name, description, requires_admin: bool, icon: string /* SF Symbol */ }
TaskResult { id, ok: bool, output: string, error?: string }
ProcessInfo { pid: i32, name, path?: string, memory: u64 /* phys_footprint */, cpu_time_ns: u64, user_owned: bool }
```

## My Clutter and Space Lens (`clutter`)

```c
char *mcl_large_files(const char *request_json);  /* {roots?:[string], min_bytes?:u64, older_than_days?:u32, limit?:u32, include_library?:bool} -> LargeFilesReport */
char *mcl_duplicates(const char *request_json);   /* {roots?:[string], min_bytes?:u64, limit?:u32} -> DuplicatesReport */
char *mcl_space_tree(const char *path, uint32_t depth, uint32_t top); /* -> SpaceNode */
char *mcl_trash_paths(const char *request_json);  /* {paths:[string], dry_run?:bool} -> CleanReport */
```

```text
FileInfo { path, bytes: u64, modified: u64, accessed: u64,
           kind: "video"|"image"|"audio"|"archive"|"disk_image"|"document"|"app"|"code"|"other" }
LargeFilesReport { files: [FileInfo], scanned_files: u64, elapsed_ms: u64 }
DupGroup { bytes: u64 /* size of each copy */, paths: [string], wasted_bytes: u64 }
DuplicatesReport { groups: [DupGroup], total_wasted: u64, scanned_files: u64, elapsed_ms: u64 }
SpaceNode { name, path, bytes: u64, files: u64, is_dir: bool, children: [SpaceNode], others_bytes: u64 }
```

## Privacy and extended cleanup (`privacy`, `rules`)

```c
char *mcl_privacy_scan(void);                     /* -> PrivacyReport */
char *mcl_privacy_clean(const char *request_json);/* {items:[{source:string,item:string}], dry_run?:bool} -> CleanReport */
```

```text
PrivacyItem { id: "history"|"cookies"|"cache"|"downloads"|"autofill"|"local_storage"|"sessions"|"recent_items",
              name, paths: [string], bytes: u64, accessible: bool }
PrivacySource { id: "safari"|"chrome"|"firefox"|"edge"|"brave"|"arc"|"opera"|"vivaldi"|"system",
                name, running: bool, items: [PrivacyItem] }
PrivacyReport { sources: [PrivacySource] }
```

The new junk categories appear in the existing `ScanReport` (`mcl_scan`), without changing its shape.
