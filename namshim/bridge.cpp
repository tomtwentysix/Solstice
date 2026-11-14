#include "solstice_nam.h"

#include <algorithm>
#include <cstdio>
#include <exception>
#include <memory>
#include <stdexcept>
#include <vector>

#include "NAM/get_dsp.h"

struct SnModel {
    std::unique_ptr<nam::DSP> dsp;
    std::vector<float> in;
    int32_t max_block;
};

extern "C" SnModel* sn_load(const char* path, double sample_rate, int32_t max_block, char* err, size_t err_len) {
    try {
        auto dsp = nam::get_dsp(std::filesystem::path(path));
        if (!dsp)
            throw std::runtime_error("unsupported capture");
        if (dsp->NumInputChannels() != 1 || dsp->NumOutputChannels() != 1)
            throw std::runtime_error("only mono captures are supported");
        if (sample_rate <= 0)
            sample_rate = dsp->GetExpectedSampleRate() > 0 ? dsp->GetExpectedSampleRate() : 48000.0;
        dsp->Reset(sample_rate, max_block);
        return new SnModel{std::move(dsp), std::vector<float>(max_block), max_block};
    } catch (const std::exception& e) {
        if (err && err_len)
            snprintf(err, err_len, "%s", e.what());
    } catch (...) {
        if (err && err_len)
            snprintf(err, err_len, "the capture could not be loaded");
    }
    return nullptr;
}

extern "C" void sn_free(SnModel* m) try {
    delete m;
} catch (...) {
}

// No exception may unwind into Rust.
extern "C" double sn_expected_rate(const SnModel* m) try {
    return m->dsp->GetExpectedSampleRate();
} catch (...) {
    return 0;
}

extern "C" void sn_process(SnModel* m, const float* in, float* out, int32_t frames) try {
    if (frames > m->max_block)
        frames = m->max_block;
    // NAM takes non-const input, so copy rather than cast away const.
    std::copy(in, in + frames, m->in.data());
    float* ip = m->in.data();
    m->dsp->process(&ip, &out, frames);
} catch (...) {
    std::fill(out, out + frames, 0.0f);
}
