/* Development leftovers: project artifacts, toolchain caches and command-based cleanups. */
char *mcl_dev_projects(const char *request_json); /* {"roots"?:[string]} -> ProjectsReport */
char *mcl_dev_toolchains(void);                   /* -> ToolchainReport */
char *mcl_dev_clean(const char *request_json);    /* {"paths":[string],"dry_run"?:bool} -> CleanReport */
char *mcl_dev_run(const char *ids_json);          /* ["homebrew_cleanup","docker_prune"] -> [CommandResult] */
