/* Privacy and extended cleanup. Included from maccleaner.h (inside its extern "C").
   See docs/ffi-contracts.md. The new junk categories come out of mcl_scan. */

#ifndef MACCLEANER_PRIVACY_H
#define MACCLEANER_PRIVACY_H

/* -> PrivacyReport */
char *mcl_privacy_scan(void);
/* request_json: {"items":[{"source":string,"item":string}],"dry_run"?:bool} -> CleanReport.
   dry_run defaults to true. Paths are computed by the core; the client never sends paths. */
char *mcl_privacy_clean(const char *request_json);

#endif
