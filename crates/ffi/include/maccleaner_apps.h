/* Applications (contract in docs/ffi-contracts.md). Included from maccleaner.h,
   inside extern "C". All return the JSON envelope and are freed with mcl_string_free. */
#ifndef MACCLEANER_APPS_H
#define MACCLEANER_APPS_H

char *mcl_apps_list(void);                         /* -> [AppInfo] sorted by name */
char *mcl_apps_leftovers(const char *app_path);    /* -> AppLeftovers */
char *mcl_apps_orphans(void);                      /* -> [OrphanGroup] */
char *mcl_apps_uninstall(const char *request_json);/* {app_path?:string, leftover_paths:[string], dry_run?:bool} -> CleanReport */
char *mcl_extensions_list(void);                   /* -> [ExtensionItem] */
char *mcl_extensions_remove(const char *request_json); /* {paths:[string], dry_run?:bool} -> CleanReport */

#endif
