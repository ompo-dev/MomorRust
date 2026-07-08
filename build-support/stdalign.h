/* ponytail: shim de stdalign.h para MSVC 2019 Build Tools (14.29), que não
 * fornece este header C11. Necessário para compilar os headers C do wasmtime
 * (tree-sitter feature "wasm"). Injetado via CFLAGS em .cargo/config.toml.
 * Remova quando o toolchain for VS2022+. */
#ifndef _STDALIGN_H_SHIM
#define _STDALIGN_H_SHIM

#ifndef __cplusplus
#define alignas(x) __declspec(align(x))
#define alignof(x) __alignof(x)
#define __alignas_is_defined 1
#define __alignof_is_defined 1
#endif

#endif
