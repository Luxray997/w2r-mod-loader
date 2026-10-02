/* Compiled as C11, C++17 and (if available) 32-bit by tools/check_abi.sh. Never run. */
#include <stddef.h>
#include "w2r_abi.h"

#ifdef __cplusplus
#define CHECK(c) static_assert(c, #c)
#else
#define CHECK(c) _Static_assert(c, #c)
#endif

CHECK(W2R_ABI_VERSION == 1);
CHECK(offsetof(W2rHost, struct_size) == 0);
CHECK(offsetof(W2rHost, abi_version) == 4);
CHECK(offsetof(W2rHost, token) == 8);
CHECK(offsetof(W2rPlugin, struct_size) == 0);
CHECK(offsetof(W2rUi, struct_size) == 0);
CHECK(offsetof(W2rEvent, struct_size) == 0);
CHECK(offsetof(W2rStr, len) == sizeof(void *));
CHECK(sizeof(W2rStatus) == 4);
CHECK(W2R_OK == 0);
CHECK(W2R_E_DENIED < 0 && W2R_E_CONFLICT < 0);
CHECK(W2R_MAX_PEER_PAYLOAD + W2R_PEER_HEADER_BYTES <= 989);

/* The smallest possible plugin must compile against the header. */
static W2rStatus W2R_CALL on_load(const W2rHost *host) {
    W2rStr msg = {"hello", 5};
    return host->log(host->token, W2R_LOG_INFO, msg);
}
static const W2rPlugin kPlugin = {sizeof(W2rPlugin), on_load, 0, 0, 0};

W2R_EXPORT const W2rPlugin *W2R_CALL w2r_plugin_main(const W2rHost *host) {
    return host->abi_version >= 1 ? &kPlugin : 0;
}
