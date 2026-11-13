#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SnModel SnModel;

SnModel* sn_load(const char* path, double sample_rate, int32_t max_block, char* err, size_t err_len);
void sn_free(SnModel* m);
// The training rate, or -1 when the capture does not say.
double sn_expected_rate(const SnModel* m);
// in and out may alias; frames must not exceed the max_block given to sn_load.
void sn_process(SnModel* m, const float* in, float* out, int32_t frames);

#ifdef __cplusplus
}
#endif
