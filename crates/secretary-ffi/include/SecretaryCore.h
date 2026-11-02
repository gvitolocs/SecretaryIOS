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
 * - Every module follows the same pattern: `secretary_<module>_open(path)`,
 *   `secretary_<module>_request(handle, json)` (JSON in, JSON out, see
 *   docs/server-api.md for the command protocol), `secretary_<module>_close`.
 *   `secretary_version()` lists the modules compiled in.
 * - A handle must not be closed while a request is in flight on it, and a
 *   handle must not be used after it has been closed.
 */

#include <stdint.h>

#include <stdbool.h>

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SecretaryGymEngine SecretaryGymEngine;
typedef struct SecretaryBodyEngine SecretaryBodyEngine;
typedef struct SecretaryPlayerEngine SecretaryPlayerEngine;
typedef struct SecretaryVaultEngine SecretaryVaultEngine;

char *secretary_open_error(void);
void secretary_string_free(char *value);
char *secretary_version(void);

SecretaryGymEngine *secretary_gym_open(const char *path);
char *secretary_gym_request(SecretaryGymEngine *handle, const char *json);
void secretary_gym_close(SecretaryGymEngine *handle);

/* Body weight (secretary-body). */
SecretaryBodyEngine *secretary_body_open(const char *path);
char *secretary_body_request(SecretaryBodyEngine *handle, const char *json);
void secretary_body_close(SecretaryBodyEngine *handle);

/* Media player core (secretary-player): queue, resume positions, subtitles, playback plans. */
SecretaryPlayerEngine *secretary_player_open(const char *path);
char *secretary_player_request(SecretaryPlayerEngine *handle, const char *json);
void secretary_player_close(SecretaryPlayerEngine *handle);

/* An Obsidian vault as plain files (lucina-vault): `path` is the vault's folder, read-only until {"op":"set_writable","writable":true}.
   Every answer has "ok"; a failure adds "code" and "error". Operations: docs/lucina-vault.md. */
SecretaryVaultEngine *secretary_vault_open(const char *path);
char *secretary_vault_request(SecretaryVaultEngine *handle, const char *json);
void secretary_vault_close(SecretaryVaultEngine *handle);

/* Image viewer core (secretary-image): ThumbHash placeholders, progressive JPEG XL decoding as
   bytes arrive, zoom/pan. Codes: >= 0 ok (1 = something new), < 0 error (secretary_image_last_error). */
typedef struct SecretaryImageDecoder SecretaryImageDecoder;
char *secretary_image_last_error(void);
SecretaryImageDecoder *secretary_image_decoder_new(void);
int32_t secretary_image_decoder_feed(SecretaryImageDecoder *decoder, const uint8_t *bytes, size_t len, bool done);
int32_t secretary_image_decoder_frame(const SecretaryImageDecoder *decoder, uint32_t *width, uint32_t *height,
                                      const uint8_t **rgba, bool *complete);
int32_t secretary_image_decoder_size(const SecretaryImageDecoder *decoder, uint32_t *width, uint32_t *height);
void secretary_image_decoder_free(SecretaryImageDecoder *decoder);
int32_t secretary_image_thumbhash(const uint8_t *hash, size_t len, uint32_t *width, uint32_t *height,
                                  uint8_t **rgba, size_t *rgba_len);
void secretary_image_buffer_free(uint8_t *rgba, size_t len);
char *secretary_image_view(const char *json);

/* Chat engine kernel (secretary-chat): the timeline merge and order, the sync cursor loop and the pagination rules
   of ios/Secretary/Features/Chat/{Timeline,ChatSync,ChatStore}.swift. Stateless: no handle, one JSON request in, one
   JSON response out (`{"error":"..."}` for anything it cannot answer). Free the result with `secretary_string_free`.
   Requests (full protocol: docs/server-api.md, "Chat engine kernel"):
     {"op":"merge","current":[Key],"incoming":[Key]}
         -> {"ids":[..],"replaced":[{"local","server"}],"reply_to_updates":[{"id","reply_to"}]}
         Key = {"id","ts_us","role":"user"|"lucina","client_message_id"?,"reply_to"?,"share_id"?}
     {"op":"sync_next","plan":null,"cursor":"c0"|null,"outcome":{"kind":"started"}}   (first call; later: "plan":<last plan>)
         outcome = {"kind":"started"} | {"kind":"page","cursor":"c1"|null,"has_more":true} | {"kind":"applied"}
                 | {"kind":"gone410"} | {"kind":"failed"}
         -> {"plan":<opaque>,"action":{"kind":"fetch","after":"c0"|null,"limit":200} | {"kind":"apply","reset":false}
                                    | {"kind":"done"} | {"kind":"abort"}}
     {"op":"older","ids":[..],"stored":null|"..","page_len":60,"page_size":60}
         -> {"older_cursor":"tl_3"|null,"has_older":true}
   The rules are proved in verification/verus/chat_merge.rs. */
char *secretary_chat_request(const char *json);

/* Compatibility aliases for the SwiftUI port. */
char *secretary_gym_open_error(void);
void secretary_gym_string_free(char *value);

#ifdef __cplusplus
}
#endif

#endif
