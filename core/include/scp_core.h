/* C ABI for the shared transfer core. Matches core/src/ffi.rs.
 *
 * Memory rules:
 *   - Strings passed in are NUL-terminated UTF-8.
 *   - scp_list_dir returns a heap string you must release with scp_string_free.
 *   - scp_last_error returns a borrowed pointer; do not free it.
 */
#ifndef SCP_CORE_H
#define SCP_CORE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ScpSession ScpSession;

/* Error codes returned by scp_last_error_code. */
#define SCP_ERR_NONE 0
#define SCP_ERR_GENERIC 1
#define SCP_ERR_UNKNOWN_HOST_KEY 2
#define SCP_ERR_HOST_KEY_MISMATCH 3

/* protocol: 0=SFTP, 1=FTP, 2=FTPS, 3=S3. Returns NULL on failure.
 *
 * bucket/region apply to S3 only; empty/NULL means absent. host_key_mode:
 * 0 = strict (fail with SCP_ERR_UNKNOWN_HOST_KEY on new servers),
 * 1 = trust-on-first-use, 2 = accept only if the server's SHA256 fingerprint
 * equals expected_fingerprint (obtained from scp_last_fingerprint after a
 * strict connect failed). */
ScpSession *scp_connect(int protocol,
                        const char *host,
                        uint16_t port,
                        const char *username,
                        const char *password,
                        const char *bucket,
                        const char *region,
                        int host_key_mode,
                        const char *expected_fingerprint);

/* Classify the last error on this thread (SCP_ERR_*). */
int scp_last_error_code(void);

/* Fingerprint from the last host-key error, or NULL. Borrowed - do not free. */
const char *scp_last_fingerprint(void);

/* Returns a JSON array string (free with scp_string_free), or NULL on error. */
char *scp_list_dir(ScpSession *session, const char *path);

/* Returns bytes transferred, or -1 on error. */
int64_t scp_download(ScpSession *session, const char *remote, const char *local);
int64_t scp_upload(ScpSession *session, const char *local, const char *remote);

/* Progress callback: (transferred, total, user_data). total is 0 if unknown. */
typedef void (*ScpProgressCb)(uint64_t transferred, uint64_t total, void *user_data);

/* Transfer with progress reporting. cb runs on the calling thread; user_data
 * is passed back verbatim. Returns bytes transferred, or -1 on error. */
int64_t scp_download_cb(ScpSession *session, const char *remote, const char *local,
                        ScpProgressCb cb, void *user_data);
int64_t scp_upload_cb(ScpSession *session, const char *local, const char *remote,
                      ScpProgressCb cb, void *user_data);

/* Closes the session and frees the handle. Safe to pass NULL. */
void scp_disconnect_free(ScpSession *session);

/* Last error on this thread, or NULL. Borrowed - do not free. */
const char *scp_last_error(void);

/* Frees a string returned by the core. */
void scp_string_free(char *s);

#ifdef __cplusplus
}
#endif

#endif /* SCP_CORE_H */
