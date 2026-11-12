#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SvPlugin SvPlugin;

typedef void (*SvScanCallback)(void* ctx, const char* uid, const char* name, const char* vendor,
                               const char* subcategories);

typedef struct {
    uint32_t frame;
    uint8_t data[3];
} SvMidi;

typedef struct {
    uint32_t id;
    double value;
} SvParam;

typedef struct {
    int64_t position;
    double tempo;
    int32_t sig_num;
    int32_t sig_den;
    int32_t playing;
} SvContext;

typedef struct {
    uint32_t id;
    char title[128];
    char units[32];
    double default_value;
    int32_t steps;
    int32_t flags;
} SvParamInfo;

/* Lists the audio processor classes in a .vst3 bundle. Returns the count, or -1 on error. */
int sv_scan(const char* path, SvScanCallback cb, void* ctx, char* err, size_t err_len);

SvPlugin* sv_open(const char* path, const char* uid, double sample_rate, int32_t max_block, char* err,
                  size_t err_len);
void sv_close(SvPlugin* p);

/* RT thread only. Buffers are stereo; `in` may alias `out`. */
/* `params` are host automation values, applied at the start of the block. */
void sv_process(SvPlugin* p, const float* const* in, float* const* out, int32_t frames, const SvMidi* events,
                int32_t n_events, const SvParam* params, int32_t n_params, const SvContext* ctx);

uint32_t sv_latency(SvPlugin* p);
int32_t sv_has_input(SvPlugin* p);

/* State is returned in a buffer owned by the plugin, valid until the next call. */
int32_t sv_get_state(SvPlugin* p, const uint8_t** data, size_t* len);
int32_t sv_set_state(SvPlugin* p, const uint8_t* data, size_t len);

int32_t sv_param_count(SvPlugin* p);
int32_t sv_param_info(SvPlugin* p, int32_t index, SvParamInfo* out);
double sv_param_get(SvPlugin* p, uint32_t id);
void sv_param_set(SvPlugin* p, uint32_t id, double value);
void sv_param_string(SvPlugin* p, uint32_t id, double value, char* out, size_t len);

int32_t sv_has_editor(SvPlugin* p);
int32_t sv_open_editor(SvPlugin* p, const char* title);
/* The editor inside the X11 window `parent` at (x, y), in its device pixels. */
int32_t sv_open_editor_in(SvPlugin* p, unsigned long parent, int32_t x, int32_t y);
void sv_editor_size(SvPlugin* p, int32_t* w, int32_t* h);
void sv_move_editor(SvPlugin* p, int32_t x, int32_t y);
void sv_close_editor(SvPlugin* p);
int32_t sv_editor_open(SvPlugin* p);

/* Main thread, roughly every 16 ms: plugin run loop, editor windows, output parameter changes. */
void sv_idle(void);

#ifdef __cplusplus
}
#endif
