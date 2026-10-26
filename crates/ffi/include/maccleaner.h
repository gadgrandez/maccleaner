#ifndef MACCLEANER_H
#define MACCLEANER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Every function returning `char *` hands back a JSON
   {"ok":bool,"data":...,"error":string|null} that must be freed with
   mcl_string_free. They are blocking: call them off the main thread. */

const char *mcl_version(void);

char *mcl_scan(void);
/* request_json: {"paths":["/abs/path",...],"dry_run":bool}; dry_run defaults to true. */
char *mcl_clean(const char *request_json);
/* on != 0: every cleanup deletes for good instead of moving to the Trash (default off).
   -> "trash" | "delete" */
char *mcl_set_delete_permanently(int on);
/* Progress of the cleanup in course, from any module:
   {"active":bool,"done":u64,"total":u64 (0 = unknown),"bytes":u64,"current":string|null} */
char *mcl_clean_progress(void);
char *mcl_space(const char *path, uint32_t top);

char *mcl_system_status(void);
/* {"full_disk_access":bool}: without it, Desktop/Documents/Downloads are not scanned. */
char *mcl_access_status(void);

char *mcl_monitor_status(void);
char *mcl_monitor_install(const char *program_path);
char *mcl_monitor_uninstall(void);
char *mcl_monitor_info(void);

void mcl_string_free(char *s);

#include "maccleaner_apps.h"
#include "maccleaner_performance.h"
#include "maccleaner_clutter.h"
#include "maccleaner_privacy.h"
#include "maccleaner_dev.h"

#ifdef __cplusplus
}
#endif

#endif
