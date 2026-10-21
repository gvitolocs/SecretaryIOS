#ifndef SECRETARY_CORE_H
#define SECRETARY_CORE_H

/*
 * SecretaryCore: C ABI for the Secretary iOS app.
 *
 * Ownership:
 * - Every returned `char *` is owned by the caller and must be released with
 *   `secretary_string_free` (the `secretary_gym_*` aliases work too).
 * - Open functions return NULL on error. Call `secretary_open_error` and free
 *   the returned string to get the message.
 * - A handle must not be closed while a request is in flight on it, and a
 *   handle must not be used after it has been closed.
 */

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SecretaryGymEngine SecretaryGymEngine;

char *secretary_open_error(void);
void secretary_string_free(char *value);
char *secretary_version(void);

SecretaryGymEngine *secretary_gym_open(const char *path);
char *secretary_gym_request(SecretaryGymEngine *handle, const char *json);
void secretary_gym_close(SecretaryGymEngine *handle);

/* Compatibility aliases for the SwiftUI port. */
char *secretary_gym_open_error(void);
void secretary_gym_string_free(char *value);

#ifdef __cplusplus
}
#endif

#endif
