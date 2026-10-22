#ifndef MACCLEANER_PERFORMANCE_H
#define MACCLEANER_PERFORMANCE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Performance. Same JSON envelope as maccleaner.h; free with mcl_string_free.
   LaunchItem { label, plist_path, program?, scope: "user_agent"|"global_agent"|"global_daemon",
                run_at_load, keep_alive, enabled, loaded, pid?, app_name?, removable }
   MaintenanceTask { id, name, description, requires_admin, icon }
   TaskResult { id, ok, output, error? }
   ProcessInfo { pid, name, path?, memory, cpu_time_ns, user_owned } */

char *mcl_launch_items(void);                                      /* -> [LaunchItem] */
char *mcl_launch_item_set_enabled(const char *label, int enabled); /* user_agent only -> LaunchItem */
char *mcl_launch_item_remove(const char *plist_path, int dry_run); /* ~/Library/LaunchAgents only -> CleanReport */
char *mcl_maintenance_tasks(void);                                 /* -> [MaintenanceTask] */
/* ids_json: ["flush_dns",...]. Admin tasks ask for the password in a single dialog. -> [TaskResult] */
char *mcl_maintenance_run(const char *ids_json);
char *mcl_top_processes(uint32_t limit);                           /* -> [ProcessInfo] by memory desc.; 0 = all */

#ifdef __cplusplus
}
#endif

#endif
