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
/* Network-level failure (connect / IO / protocol) — a reconnect may help. */
#define SCP_ERR_NETWORK 4

/* protocol: 0=SFTP, 1=FTP, 2=FTPS, 3=S3. Returns NULL on failure.
 *
 * bucket/region apply to S3 only; empty/NULL means absent. host_key_mode:
 * 0 = strict (fail with SCP_ERR_UNKNOWN_HOST_KEY on new servers),
 * 1 = trust-on-first-use, 2 = accept only if the server's SHA256 fingerprint
 * equals expected_fingerprint (obtained from scp_last_fingerprint after a
 * strict connect failed).
 * auth_mode: 0 = password, 1 = key file (key_path set, password acts as the
 * passphrase), 2 = ssh-agent. */
ScpSession *scp_connect(int protocol,
                        const char *host,
                        uint16_t port,
                        const char *username,
                        const char *password,
                        const char *bucket,
                        const char *region,
                        int host_key_mode,
                        const char *expected_fingerprint,
                        int auth_mode,
                        const char *key_path);

/* Classify the last error on this thread (SCP_ERR_*). */
int scp_last_error_code(void);

/* Fingerprint from the last host-key error, or NULL. Borrowed - do not free. */
const char *scp_last_fingerprint(void);

/* Returns a JSON array string (free with scp_string_free), or NULL on error.
 * Each entry: {"name":...,"is_dir":bool,"size":N,"mtime":N|null,
 *              "perms":"rwxr-xr-x"|null,"is_symlink":bool,
 *              "uid":N|null,"gid":N|null} */
char *scp_list_dir(ScpSession *session, const char *path);

/* Returns bytes transferred, or -1 on error. */
int64_t scp_download(ScpSession *session, const char *remote, const char *local);
int64_t scp_upload(ScpSession *session, const char *local, const char *remote);

/* Progress callback: (transferred, total, user_data). total is 0 if unknown.
 * Return 0 to continue, non-zero to cancel (the call fails with "cancelled"). */
typedef int (*ScpProgressCb)(uint64_t transferred, uint64_t total, void *user_data);

/* Transfer with progress reporting. cb runs on the calling thread; user_data
 * is passed back verbatim. Returns bytes transferred, or -1 on error. */
int64_t scp_download_cb(ScpSession *session, const char *remote, const char *local,
                        ScpProgressCb cb, void *user_data);
int64_t scp_upload_cb(ScpSession *session, const char *local, const char *remote,
                      ScpProgressCb cb, void *user_data);

/* Multi-file operation callback. kind: 0 = starting `file` (total bytes; done
 * is 1 for a download, 0 for an upload), 1 = byte progress for the current
 * file (file is NULL), 2 = current file finished. Return 0 to continue,
 * non-zero to cancel the whole operation. */
typedef int (*ScpXferCb)(int kind, const char *file, uint64_t done, uint64_t total,
                         void *user_data);

/* Recursive folder transfers. Return total bytes moved, or -1 on error.
 * excludes: ";"-separated WinSCP-style masks ("*.tmp; .git/"); NULL/empty = none. */
int64_t scp_download_dir(ScpSession *session, const char *remote, const char *local,
                         const char *excludes, ScpXferCb cb, void *user_data);
int64_t scp_upload_dir(ScpSession *session, const char *local, const char *remote,
                       const char *excludes, ScpXferCb cb, void *user_data);

/* One-way directory sync. direction: 0 = local->remote, 1 = remote->local.
 * delete_extraneous: non-zero enables mirror mode (removes destination items
 * that have no source counterpart). Returns files copied, or -1 on error. */
int64_t scp_sync_dir(ScpSession *session, const char *local, const char *remote,
                     int direction, const char *excludes, int delete_extraneous,
                     ScpXferCb cb, void *user_data);

/* Sync dry run: returns JSON
 * {"dirs":[...],"items":[{"rel","size","reason"}],"deletes":[...]}
 * (free with scp_string_free) or NULL on error. Copies nothing.
 * delete_extraneous: non-zero populates the "deletes" array. */
char *scp_sync_plan(ScpSession *session, const char *local, const char *remote,
                    int direction, const char *excludes, int delete_extraneous);

/* Recursive remote search by mask ("*.log"). Returns JSON
 * [{"path","is_dir","size"}] (free with scp_string_free), or NULL. */
char *scp_find(ScpSession *session, const char *base, const char *mask, uint32_t limit);

/* Resume an upload: appends the local tail after the remote file's current
 * size. Returns total remote bytes afterwards, or -1. */
int64_t scp_upload_resume_cb(ScpSession *session, const char *local, const char *remote,
                             ScpProgressCb cb, void *user_data);

/* Remote file management. Return 0 on success, -1 on error. */
int scp_mkdir(ScpSession *session, const char *path);
int scp_remove_file(ScpSession *session, const char *path);
int scp_remove_dir_all(ScpSession *session, const char *path);
int scp_rename(ScpSession *session, const char *from, const char *to);
/* mode is the unix permission bits, e.g. 0644. SFTP and FTP (SITE CHMOD). */
int scp_chmod(ScpSession *session, const char *path, uint32_t mode);

/* Resume a download from `offset` bytes, appending to the local file.
 * Progress reports overall position. Returns total bytes, or -1 on error. */
int64_t scp_download_resume_cb(ScpSession *session, const char *remote,
                               const char *local, uint64_t offset,
                               ScpProgressCb cb, void *user_data);

/* Liveness probe / NAT keepalive. 0 = alive, -1 = session appears dead
 * (the next operation will transparently reconnect and retry once). */
int scp_keepalive(ScpSession *session);

/* Execute a remote command (SFTP/SSH sessions only). Returns a JSON string
 * {"exit_code":N,"stdout":"...","stderr":"..."} (free with scp_string_free),
 * or NULL on error. */
char *scp_exec_command(ScpSession *session, const char *cmd);

/* Server-side file copy. Returns bytes copied, or -1 on error. */
int64_t scp_copy_file(ScpSession *session, const char *src, const char *dst);

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
