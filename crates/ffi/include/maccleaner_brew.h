/* Homebrew packages: listed and removed through brew itself. */
char *mcl_brew_packages(void);                       /* -> BrewReport */
char *mcl_brew_uninstall(const char *request_json);  /* {"name":string,"kind":"formula"|"cask"} -> true */
char *mcl_brew_autoremove(void);                     /* -> true */
