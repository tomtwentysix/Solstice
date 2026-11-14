#include "solstice_vst3.h"
#include "x11window.h"

#include "pluginterfaces/base/funknownimpl.h"
#include "pluginterfaces/base/ibstream.h"
#include "pluginterfaces/gui/iplugview.h"
#include "pluginterfaces/vst/ivstaudioprocessor.h"
#include "pluginterfaces/vst/ivstcomponent.h"
#include "pluginterfaces/vst/ivsteditcontroller.h"
#include "pluginterfaces/vst/ivstevents.h"
#include "pluginterfaces/vst/ivstmessage.h"
#include "pluginterfaces/vst/ivstmidicontrollers.h"
#include "pluginterfaces/vst/ivstprocesscontext.h"
#include "pluginterfaces/vst/vstspeaker.h"
#include "public.sdk/source/vst/hosting/eventlist.h"
#include "public.sdk/source/vst/hosting/hostclasses.h"
#include "public.sdk/source/vst/hosting/module.h"
#include "public.sdk/source/vst/hosting/parameterchanges.h"
#include "public.sdk/source/vst/utility/stringconvert.h"

#include <poll.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstring>
#include <map>
#include <set>
#include <string>
#include <thread>
#include <vector>

using namespace Steinberg;
using namespace Steinberg::Vst;

static HostApplication* host() {
    static HostApplication* app = new HostApplication;
    return app;
}

// Never destroyed: unloading modules at exit calls into plug-ins that may still be open,
// or whose bridge host has died, and crashes the process as it ends.
static std::map<std::string, VST3::Hosting::Module::Ptr>& modules() {
    static auto* m = new std::map<std::string, VST3::Hosting::Module::Ptr>;
    return *m;
}

static void set_err(char* err, size_t len, const std::string& msg) {
    if (err && len)
        snprintf(err, len, "%s", msg.c_str());
}

static std::string utf8(const TChar* s) {
    return VST3::StringConvert::convert(std::u16string(reinterpret_cast<const char16_t*>(s)));
}

static VST3::Hosting::Module::Ptr load_module(const char* path, char* err, size_t err_len) {
    auto it = modules().find(path);
    if (it != modules().end())
        return it->second;
    std::string e;
    auto m = VST3::Hosting::Module::create(path, e);
    if (!m) {
        set_err(err, err_len, e.empty() ? "cannot load module" : e);
        return nullptr;
    }
    m->getFactory().setHostContext(host());
    modules()[path] = m;
    return m;
}

struct MemStream : U::Implements<U::Directly<IBStream>> {
    std::vector<uint8_t> data;
    int64 pos = 0;

    tresult PLUGIN_API read(void* buffer, int32 n, int32* done) override {
        int32 avail = static_cast<int32>(std::max<int64>(0, static_cast<int64>(data.size()) - pos));
        int32 k = std::min(n, avail);
        if (k > 0)
            memcpy(buffer, data.data() + pos, k);
        pos += k;
        if (done)
            *done = k;
        return kResultOk;
    }
    tresult PLUGIN_API write(void* buffer, int32 n, int32* done) override {
        if (pos + n > static_cast<int64>(data.size()))
            data.resize(pos + n);
        memcpy(data.data() + pos, buffer, n);
        pos += n;
        if (done)
            *done = n;
        return kResultOk;
    }
    tresult PLUGIN_API seek(int64 p, int32 mode, int64* result) override {
        int64 base = mode == kIBSeekSet ? 0 : mode == kIBSeekCur ? pos : static_cast<int64>(data.size());
        pos = std::max<int64>(0, base + p);
        if (result)
            *result = pos;
        return kResultOk;
    }
    tresult PLUGIN_API tell(int64* p) override {
        if (p)
            *p = pos;
        return kResultOk;
    }
};

struct Plugin;

struct RunLoop {
    struct Timer {
        Linux::ITimerHandler* handler;
        int64_t interval;
        int64_t next;
    };
    std::vector<std::pair<Linux::IEventHandler*, int>> fds;
    std::vector<Timer> timers;

    static int64_t now() {
        using namespace std::chrono;
        return duration_cast<milliseconds>(steady_clock::now().time_since_epoch()).count();
    }

    void run() {
        // Handlers may unregister themselves while being called, so iterate over copies.
        auto fd_copy = fds;
        std::vector<pollfd> pfds;
        for (auto& [h, fd] : fd_copy)
            pfds.push_back({fd, POLLIN, 0});
        if (!pfds.empty() && poll(pfds.data(), pfds.size(), 0) > 0) {
            for (size_t i = 0; i < pfds.size(); i++) {
                bool still = std::find(fds.begin(), fds.end(), fd_copy[i]) != fds.end();
                if (still && (pfds[i].revents & (POLLIN | POLLHUP | POLLERR)))
                    fd_copy[i].first->onFDIsSet(fd_copy[i].second);
            }
        }
        auto timer_copy = timers;
        int64_t t = now();
        for (auto& tm : timer_copy) {
            auto it = std::find_if(timers.begin(), timers.end(),
                                   [&](const Timer& x) { return x.handler == tm.handler; });
            if (it == timers.end() || t < it->next)
                continue;
            it->next = t + it->interval;
            tm.handler->onTimer();
        }
    }
};

static RunLoop& run_loop() {
    static RunLoop rl;
    return rl;
}

struct Frame : U::Implements<U::Directly<IPlugFrame, Linux::IRunLoop>> {
    Plugin* plugin;
    std::vector<Linux::IEventHandler*> own_fds;
    std::vector<Linux::ITimerHandler*> own_timers;
    explicit Frame(Plugin* p) : plugin(p) {}

    tresult PLUGIN_API resizeView(IPlugView* view, ViewRect* r) override;

    tresult PLUGIN_API registerEventHandler(Linux::IEventHandler* h, Linux::FileDescriptor fd) override {
        run_loop().fds.push_back({h, fd});
        own_fds.push_back(h);
        return kResultOk;
    }
    tresult PLUGIN_API unregisterEventHandler(Linux::IEventHandler* h) override {
        auto& v = run_loop().fds;
        v.erase(std::remove_if(v.begin(), v.end(), [&](auto& e) { return e.first == h; }), v.end());
        return kResultOk;
    }
    tresult PLUGIN_API registerTimer(Linux::ITimerHandler* h, Linux::TimerInterval ms) override {
        run_loop().timers.push_back({h, static_cast<int64_t>(ms), RunLoop::now() + static_cast<int64_t>(ms)});
        own_timers.push_back(h);
        return kResultOk;
    }
    tresult PLUGIN_API unregisterTimer(Linux::ITimerHandler* h) override {
        auto& v = run_loop().timers;
        v.erase(std::remove_if(v.begin(), v.end(), [&](auto& t) { return t.handler == h; }), v.end());
        return kResultOk;
    }
};

struct Handler : U::Implements<U::Directly<IComponentHandler, IComponentHandler2>> {
    Plugin* plugin;
    explicit Handler(Plugin* p) : plugin(p) {}
    tresult PLUGIN_API beginEdit(ParamID) override { return kResultOk; }
    tresult PLUGIN_API performEdit(ParamID id, ParamValue v) override;
    tresult PLUGIN_API endEdit(ParamID) override { return kResultOk; }
    tresult PLUGIN_API restartComponent(int32 flags) override;
    tresult PLUGIN_API setDirty(TBool state) override;
    tresult PLUGIN_API requestOpenEditor(FIDString) override { return kNotImplemented; }
    tresult PLUGIN_API startGroupEdit() override { return kResultOk; }
    tresult PLUGIN_API finishGroupEdit() override { return kResultOk; }
};

struct BusSet {
    std::vector<AudioBusBuffers> buses;
    std::vector<std::vector<float>> scratch;
    std::vector<std::vector<float*>> ptrs;
    int main = -1;
    int main_channels = 0;
    int aux = -1;
};

struct Plugin {
    VST3::Hosting::Module::Ptr module;
    IPtr<IComponent> component;
    IPtr<IAudioProcessor> processor;
    IPtr<IEditController> controller;
    bool single = false;
    IPtr<IConnectionPoint> cp_component, cp_controller;
    IPtr<Handler> handler;
    ParameterChangeTransfer to_proc{1024}, from_proc{1024};
    ParameterChanges in_changes{256}, out_changes{256};
    EventList events{1024};
    BusSet ins, outs;
    std::vector<std::vector<float>> in_copy;
    ProcessContext context{};
    double sample_rate = 48000;
    int32 max_block = 0;
    int64 midi_map[16][130];
    bool has_events = false;
    IPtr<IPlugView> view;
    IPtr<Frame> frame;
    unsigned long window = 0;
    /// The editor sits inside the host's plug-in window rather than in a window of its own.
    bool embedded = false;
    int editor_w = 0, editor_h = 0;
    std::vector<uint8_t> state;
    // Set by the UI thread while it reactivates the plug-in; sv_process passes audio through
    // instead of touching the processor, and `busy` tells the UI thread when it may go ahead.
    std::atomic<bool> suspended{false}, busy{false};
    /// The plug-in changed itself, from its own window, since the host last asked.
    std::atomic<bool> dirty{false};
    bool aux_on = false;
    bool aux_zeroed = true;
};

static std::set<Plugin*>& plugins() {
    static std::set<Plugin*> s;
    return s;
}

tresult PLUGIN_API Handler::performEdit(ParamID id, ParamValue v) {
    plugin->to_proc.addChange(id, v, 0);
    plugin->dirty.store(true);
    return kResultOk;
}

// A preset loaded in the plug-in's own window changes its values without any edit.
tresult PLUGIN_API Handler::restartComponent(int32 flags) {
    if (flags & (kParamValuesChanged | kReloadComponent))
        plugin->dirty.store(true);
    return kResultOk;
}

tresult PLUGIN_API Handler::setDirty(TBool state) {
    if (state)
        plugin->dirty.store(true);
    return kResultOk;
}

tresult PLUGIN_API Frame::resizeView(IPlugView* view, ViewRect* r) {
    if (!r)
        return kInvalidArgument;
    if (plugin->embedded)
        x11_resize_child(plugin->window, r->getWidth(), r->getHeight());
    else
        x11_resize(plugin->window, r->getWidth(), r->getHeight());
    plugin->editor_w = r->getWidth();
    plugin->editor_h = r->getHeight();
    view->onSize(r);
    return kResultOk;
}

static void setup_buses(Plugin* p, BusDirection dir, BusSet& set, std::vector<SpeakerArrangement>& arrs) {
    int32 n = p->component->getBusCount(kAudio, dir);
    for (int32 i = 0; i < n; i++) {
        BusInfo info{};
        p->component->getBusInfo(kAudio, dir, i, info);
        SpeakerArrangement arr = SpeakerArr::kStereo;
        if (info.busType == kMain && set.main < 0)
            set.main = i;
        else
            p->processor->getBusArrangement(dir, i, arr);
        if (dir == kInput && info.busType == kAux && set.aux < 0)
            set.aux = i;
        arrs.push_back(arr);
    }
}

static void alloc_buses(Plugin* p, BusDirection dir, BusSet& set) {
    int32 n = p->component->getBusCount(kAudio, dir);
    set.buses.resize(n);
    set.ptrs.resize(n);
    for (int32 i = 0; i < n; i++) {
        SpeakerArrangement arr = SpeakerArr::kStereo;
        p->processor->getBusArrangement(dir, i, arr);
        int32 ch = SpeakerArr::getChannelCount(arr);
        if (i == set.main)
            set.main_channels = ch;
        p->component->activateBus(kAudio, dir, i, i == set.main);
        for (int32 c = 0; c < ch; c++) {
            set.scratch.emplace_back(p->max_block, 0.0f);
            set.ptrs[i].push_back(set.scratch.back().data());
        }
        set.buses[i].numChannels = ch;
        set.buses[i].silenceFlags = 0;
        set.buses[i].channelBuffers32 = set.ptrs[i].data();
    }
}

// Copies a stereo source into a bus: extra channels get silence and a mono bus the downmix.
// With no source the whole bus is silent and flagged so.
static void fill_bus(AudioBusBuffers& bus, const float* const* src, int32 frames) {
    for (int32 c = 0; c < bus.numChannels; c++) {
        float* dst = bus.channelBuffers32[c];
        if (src && c < 2)
            memcpy(dst, src[c], frames * sizeof(float));
        else
            memset(dst, 0, frames * sizeof(float));
    }
    if (src && bus.numChannels == 1)
        for (int32 i = 0; i < frames; i++)
            bus.channelBuffers32[0][i] = 0.5f * (src[0][i] + src[1][i]);
    bus.silenceFlags = src ? 0 : ~uint64(0);
}

// Every entry point catches: yabridge throws std::system_error once its Windows host has
// died, and an exception must never unwind into Rust.
extern "C" int sv_scan(const char* path, SvScanCallback cb, void* ctx, char* err, size_t err_len) try {
    auto m = load_module(path, err, err_len);
    if (!m)
        return -1;
    int count = 0;
    for (auto& ci : m->getFactory().classInfos()) {
        if (ci.category() != kVstAudioEffectClass)
            continue;
        cb(ctx, ci.ID().toString().c_str(), ci.name().c_str(), ci.vendor().c_str(), ci.subCategoriesString().c_str());
        count++;
    }
    return count;
} catch (...) { return -1; }

extern "C" SvPlugin* sv_open(const char* path, const char* uid, double sample_rate, int32_t max_block, char* err,
                             size_t err_len) try {
    auto m = load_module(path, err, err_len);
    if (!m)
        return nullptr;
    auto id = VST3::UID::fromString(std::string(uid));
    if (!id) {
        set_err(err, err_len, "bad class id");
        return nullptr;
    }
    auto p = new Plugin;
    p->module = m;
    p->sample_rate = sample_rate;
    p->max_block = max_block;
    auto fail = [&](const char* msg) -> SvPlugin* {
        set_err(err, err_len, msg);
        sv_close(reinterpret_cast<SvPlugin*>(p));
        return nullptr;
    };

    p->component = m->getFactory().createInstance<IComponent>(*id);
    if (!p->component || p->component->initialize(host()) != kResultOk)
        return fail("cannot create component");
    p->processor = U::cast<IAudioProcessor>(p->component);
    if (!p->processor)
        return fail("not an audio processor");

    p->controller = U::cast<IEditController>(p->component);
    p->single = p->controller != nullptr;
    if (!p->controller) {
        TUID cid;
        if (p->component->getControllerClassId(cid) == kResultTrue) {
            p->controller = m->getFactory().createInstance<IEditController>(VST3::UID::fromTUID(cid));
            if (p->controller && p->controller->initialize(host()) != kResultOk)
                p->controller = nullptr;
        }
    }
    if (p->controller) {
        p->handler = owned(new Handler(p));
        p->controller->setComponentHandler(p->handler);
        if (!p->single) {
            p->cp_component = U::cast<IConnectionPoint>(p->component);
            p->cp_controller = U::cast<IConnectionPoint>(p->controller);
            if (p->cp_component && p->cp_controller) {
                p->cp_component->connect(p->cp_controller);
                p->cp_controller->connect(p->cp_component);
            }
            MemStream s;
            if (p->component->getState(&s) == kResultOk) {
                s.seek(0, IBStream::kIBSeekSet, nullptr);
                p->controller->setComponentState(&s);
            }
        }
    }

    std::vector<SpeakerArrangement> in_arrs, out_arrs;
    setup_buses(p, kInput, p->ins, in_arrs);
    setup_buses(p, kOutput, p->outs, out_arrs);
    p->processor->setBusArrangements(in_arrs.data(), static_cast<int32>(in_arrs.size()), out_arrs.data(),
                                     static_cast<int32>(out_arrs.size()));
    alloc_buses(p, kInput, p->ins);
    alloc_buses(p, kOutput, p->outs);
    if (p->outs.main < 0)
        return fail("no audio output");
    p->in_copy.assign(2, std::vector<float>(max_block, 0.0f));
    if (p->ins.aux >= 0)
        fill_bus(p->ins.buses[p->ins.aux], nullptr, max_block);
    p->has_events = p->component->getBusCount(kEvent, kInput) > 0;
    if (p->has_events)
        p->component->activateBus(kEvent, kInput, 0, true);

    for (auto& ch : p->midi_map)
        std::fill(std::begin(ch), std::end(ch), -1);
    if (auto mapping = U::cast<IMidiMapping>(p->controller)) {
        for (int16 ch = 0; ch < 16; ch++)
            for (int16 cc = 0; cc < 130; cc++) {
                ParamID pid;
                if (mapping->getMidiControllerAssignment(0, ch, cc, pid) == kResultTrue)
                    p->midi_map[ch][cc] = pid;
            }
    }

    ProcessSetup setup{kRealtime, kSample32, max_block, sample_rate};
    if (p->processor->setupProcessing(setup) != kResultOk)
        return fail("plug-in rejected the processing setup");
    p->component->setActive(true);
    p->processor->setProcessing(true);
    p->context.sampleRate = sample_rate;
    plugins().insert(p);
    return reinterpret_cast<SvPlugin*>(p);
} catch (...) { set_err(err, err_len, "the plug-in failed while opening"); return nullptr; }

extern "C" void sv_close(SvPlugin* sp) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (!p)
        return;
    sv_close_editor(sp);
    plugins().erase(p);
    if (p->processor)
        p->processor->setProcessing(false);
    if (p->component)
        p->component->setActive(false);
    if (p->cp_component && p->cp_controller) {
        p->cp_component->disconnect(p->cp_controller);
        p->cp_controller->disconnect(p->cp_component);
    }
    if (p->controller) {
        p->controller->setComponentHandler(nullptr);
        if (!p->single)
            p->controller->terminate();
    }
    if (p->component)
        p->component->terminate();
    delete p;
} catch (...) {}

// The SDK's queues hold 5 points and there are 256 of them; going past either allocates,
// which the audio thread must not do. Past 4 points the newest value replaces the block's
// last point, and parameters beyond the queues wait for the next block.
static void add_change(ParameterChanges& changes, ParamID pid, int32 offset, ParamValue value) {
    int32 idx = -1;
    for (int32 i = 0; i < changes.getParameterCount(); i++)
        if (changes.getParameterData(i)->getParameterId() == pid)
            idx = i;
    IParamValueQueue* q = nullptr;
    if (idx >= 0)
        q = changes.getParameterData(idx);
    else if (changes.getParameterCount() < 256)
        q = changes.addParameterData(pid, idx);
    if (!q)
        return;
    int32 n = q->getPointCount();
    if (n >= 4) {
        int32 last;
        ParamValue old;
        q->getPoint(n - 1, last, old);
        offset = last;
    }
    q->addPoint(offset, value, idx);
}

static void midi_to_vst(Plugin* p, const SvMidi& m) {
    uint8 status = m.data[0] & 0xf0;
    int16 ch = m.data[0] & 0x0f;
    int32 offset = static_cast<int32>(m.frame);
    auto param = [&](int cc, double value) {
        int64 pid = p->midi_map[ch][cc];
        if (pid < 0)
            return;
        add_change(p->in_changes, static_cast<ParamID>(pid), offset, value);
    };
    Event e{};
    e.busIndex = 0;
    e.sampleOffset = offset;
    switch (status) {
    case 0x90:
        if (m.data[2] > 0) {
            e.type = Event::kNoteOnEvent;
            e.noteOn = {ch, static_cast<int16>(m.data[1]), 0.0f, m.data[2] / 127.0f, 0, -1};
            p->events.addEvent(e);
            break;
        }
        [[fallthrough]];
    case 0x80:
        e.type = Event::kNoteOffEvent;
        e.noteOff = {ch, static_cast<int16>(m.data[1]), m.data[2] / 127.0f, -1, 0.0f};
        p->events.addEvent(e);
        break;
    case 0xA0:
        e.type = Event::kPolyPressureEvent;
        e.polyPressure = {ch, static_cast<int16>(m.data[1]), m.data[2] / 127.0f, -1};
        p->events.addEvent(e);
        break;
    case 0xB0:
        param(m.data[1], m.data[2] / 127.0);
        break;
    case 0xD0:
        param(kAfterTouch, m.data[1] / 127.0);
        break;
    case 0xE0:
        param(kPitchBend, ((m.data[2] << 7) | m.data[1]) / 16383.0);
        break;
    }
}

extern "C" int32_t sv_process(SvPlugin* sp, const float* const* in, float* const* out, int32_t frames,
                           const SvMidi* events, int32_t n_events, const SvParam* params, int32_t n_params,
                           const SvContext* ctx) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    frames = std::min(frames, p->max_block);
    p->busy.store(true);
    if (p->suspended.load()) {
        for (int32 c = 0; c < 2; c++)
            if (in[c] != out[c])
                memmove(out[c], in[c], frames * sizeof(float));
        p->busy.store(false, std::memory_order_release);
        return 0;
    }
    p->in_changes.clearQueue();
    p->out_changes.clearQueue();
    p->events.clear();
    {
        ParamID id;
        ParamValue v;
        int32 offset;
        while (p->to_proc.getNextChange(id, v, offset))
            add_change(p->in_changes, id, offset, v);
    }
    for (int32 i = 0; i < n_params; i++) {
        add_change(p->in_changes, params[i].id, 0, params[i].value);
        // Echo to the controller so the plug-in's own interface follows the automation.
        p->from_proc.addChange(params[i].id, params[i].value, 0);
    }
    if (p->has_events)
        for (int32 i = 0; i < n_events; i++)
            midi_to_vst(p, events[i]);

    if (p->ins.main >= 0)
        fill_bus(p->ins.buses[p->ins.main], in, frames);
    auto& obus = p->outs.buses[p->outs.main];
    float* saved[2] = {nullptr, nullptr};
    for (int32 c = 0; c < std::min<int32>(2, obus.numChannels); c++) {
        saved[c] = obus.channelBuffers32[c];
        obus.channelBuffers32[c] = out[c];
    }

    auto& cx = p->context;
    cx.state = ProcessContext::kTempoValid | ProcessContext::kTimeSigValid | ProcessContext::kProjectTimeMusicValid;
    if (ctx->playing)
        cx.state |= ProcessContext::kPlaying;
    cx.projectTimeSamples = ctx->position;
    cx.tempo = ctx->tempo;
    cx.timeSigNumerator = ctx->sig_num;
    cx.timeSigDenominator = ctx->sig_den;
    cx.projectTimeMusic = ctx->position / p->sample_rate * ctx->tempo / 60.0;

    ProcessData data;
    data.processMode = kRealtime;
    data.symbolicSampleSize = kSample32;
    data.numSamples = frames;
    data.numInputs = static_cast<int32>(p->ins.buses.size());
    data.numOutputs = static_cast<int32>(p->outs.buses.size());
    data.inputs = p->ins.buses.data();
    data.outputs = p->outs.buses.data();
    data.inputParameterChanges = &p->in_changes;
    data.outputParameterChanges = &p->out_changes;
    data.inputEvents = p->has_events ? &p->events : nullptr;
    data.processContext = &cx;
    p->processor->process(data);

    for (int32 c = 0; c < 2; c++)
        if (saved[c])
            obus.channelBuffers32[c] = saved[c];
    if (obus.numChannels == 1)
        memcpy(out[1], out[0], frames * sizeof(float));
    p->from_proc.transferChangesFrom(p->out_changes);
    p->busy.store(false, std::memory_order_release);
    return 1;
} catch (...) {
    reinterpret_cast<Plugin*>(sp)->busy.store(false, std::memory_order_release);
    return 0;
}

extern "C" void sv_sidechain(SvPlugin* sp, const float* const* key, int32_t frames) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (p->ins.aux < 0)
        return;
    auto& bus = p->ins.buses[p->ins.aux];
    if (key) {
        fill_bus(bus, key, std::min(frames, p->max_block));
        p->aux_zeroed = false;
    } else if (!p->aux_zeroed) {
        fill_bus(bus, nullptr, p->max_block);
        p->aux_zeroed = true;
    }
} catch (...) {}

extern "C" int32_t sv_has_sidechain(SvPlugin* sp) try {
    return reinterpret_cast<Plugin*>(sp)->ins.aux >= 0;
} catch (...) { return 0; }

extern "C" int32_t sv_set_sidechain_active(SvPlugin* sp, int32_t on) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (p->ins.aux < 0 || (on != 0) == p->aux_on)
        return 0;
    // activateBus is only allowed while the component is inactive. One process call is short,
    // so waiting out the one in flight is cheaper than a lock the RT thread would have to take.
    p->suspended.store(true);
    while (p->busy.load())
        std::this_thread::yield();
    p->processor->setProcessing(false);
    p->component->setActive(false);
    bool ok = p->component->activateBus(kAudio, kInput, p->ins.aux, on != 0) == kResultOk;
    if (ok)
        p->aux_on = on != 0;
    // A plug-in that won't come back up stays suspended, passing audio through, rather than
    // being processed while inactive.
    bool active = p->component->setActive(true) == kResultOk;
    if (active) {
        p->processor->setProcessing(true);
        p->suspended.store(false);
    }
    return ok && active;
} catch (...) { return 0; }

extern "C" uint32_t sv_latency(SvPlugin* sp) try {
    return reinterpret_cast<Plugin*>(sp)->processor->getLatencySamples();
} catch (...) { return 0; }

extern "C" int32_t sv_has_input(SvPlugin* sp) try {
    return reinterpret_cast<Plugin*>(sp)->ins.main >= 0;
} catch (...) { return 0; }

extern "C" int32_t sv_get_state(SvPlugin* sp, const uint8_t** data, size_t* len) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    // Layout: u32 LE component-state length, component state, controller state.
    MemStream comp, ctrl;
    if (p->component->getState(&comp) != kResultOk)
        return 0;
    if (p->controller && !p->single)
        p->controller->getState(&ctrl);
    uint32_t n = static_cast<uint32_t>(comp.data.size());
    p->state.clear();
    for (int i = 0; i < 4; i++)
        p->state.push_back((n >> (8 * i)) & 0xff);
    p->state.insert(p->state.end(), comp.data.begin(), comp.data.end());
    p->state.insert(p->state.end(), ctrl.data.begin(), ctrl.data.end());
    *data = p->state.data();
    *len = p->state.size();
    return 1;
} catch (...) { return 0; }

extern "C" int32_t sv_set_state(SvPlugin* sp, const uint8_t* data, size_t len) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (len < 4)
        return 0;
    uint32_t n = data[0] | (data[1] << 8) | (data[2] << 16) | (static_cast<uint32_t>(data[3]) << 24);
    if (4 + static_cast<size_t>(n) > len)
        return 0;
    MemStream comp;
    comp.data.assign(data + 4, data + 4 + n);
    if (p->component->setState(&comp) != kResultOk)
        return 0;
    if (p->controller) {
        comp.seek(0, IBStream::kIBSeekSet, nullptr);
        p->controller->setComponentState(&comp);
        if (!p->single && len > 4 + n) {
            MemStream ctrl;
            ctrl.data.assign(data + 4 + n, data + len);
            p->controller->setState(&ctrl);
        }
    }
    return 1;
} catch (...) { return 0; }

extern "C" int32_t sv_param_count(SvPlugin* sp) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    return p->controller ? p->controller->getParameterCount() : 0;
} catch (...) { return 0; }

extern "C" int32_t sv_param_info(SvPlugin* sp, int32_t index, SvParamInfo* out) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    ParameterInfo info{};
    if (!p->controller || p->controller->getParameterInfo(index, info) != kResultOk)
        return 0;
    out->id = info.id;
    snprintf(out->title, sizeof out->title, "%s", utf8(info.title).c_str());
    snprintf(out->units, sizeof out->units, "%s", utf8(info.units).c_str());
    out->default_value = info.defaultNormalizedValue;
    out->steps = info.stepCount;
    out->flags = info.flags;
    return 1;
} catch (...) { return 0; }

extern "C" double sv_param_get(SvPlugin* sp, uint32_t id) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    return p->controller ? p->controller->getParamNormalized(id) : 0.0;
} catch (...) { return 0; }

extern "C" void sv_param_set(SvPlugin* sp, uint32_t id, double value) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (p->controller)
        p->controller->setParamNormalized(id, value);
    p->to_proc.addChange(id, value, 0);
} catch (...) {}

extern "C" void sv_param_string(SvPlugin* sp, uint32_t id, double value, char* out, size_t len) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    String128 s{};
    if (p->controller && p->controller->getParamStringByValue(id, value, s) == kResultOk)
        snprintf(out, len, "%s", utf8(s).c_str());
    else
        snprintf(out, len, "%.2f", value);
} catch (...) {}

extern "C" int32_t sv_has_editor(SvPlugin* sp) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (!p->controller)
        return 0;
    auto v = owned(p->controller->createView(ViewType::kEditor));
    return v && v->isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) == kResultTrue;
} catch (...) { return 0; }

extern "C" int32_t sv_open_editor(SvPlugin* sp, const char* title) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (p->view) {
        x11_raise(p->window);
        return 1;
    }
    if (!p->controller)
        return 0;
    p->view = owned(p->controller->createView(ViewType::kEditor));
    if (!p->view || p->view->isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) != kResultTrue) {
        p->view = nullptr;
        return 0;
    }
    ViewRect r{0, 0, 640, 400};
    p->view->getSize(&r);
    p->window = x11_create_window(std::max(r.getWidth(), 1), std::max(r.getHeight(), 1), title);
    if (!p->window) {
        p->view = nullptr;
        return 0;
    }
    p->frame = owned(new Frame(p));
    p->view->setFrame(p->frame);
    if (p->view->attached(reinterpret_cast<void*>(p->window), kPlatformTypeX11EmbedWindowID) != kResultOk) {
        sv_close_editor(sp);
        return 0;
    }
    x11_map(p->window);
    return 1;
} catch (...) { return 0; }

extern "C" int32_t sv_open_editor_in(SvPlugin* sp, unsigned long parent, int32_t x, int32_t y) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (p->view)
        return 1;
    if (!p->controller || !parent)
        return 0;
    p->view = owned(p->controller->createView(ViewType::kEditor));
    if (!p->view || p->view->isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) != kResultTrue) {
        p->view = nullptr;
        return 0;
    }
    ViewRect r{0, 0, 640, 400};
    p->view->getSize(&r);
    p->editor_w = std::max(r.getWidth(), 1);
    p->editor_h = std::max(r.getHeight(), 1);
    p->window = x11_create_child(parent, x, y, p->editor_w, p->editor_h);
    if (!p->window) {
        p->view = nullptr;
        return 0;
    }
    p->embedded = true;
    p->frame = owned(new Frame(p));
    p->view->setFrame(p->frame);
    if (p->view->attached(reinterpret_cast<void*>(p->window), kPlatformTypeX11EmbedWindowID) != kResultOk) {
        sv_close_editor(sp);
        return 0;
    }
    x11_map(p->window);
    return 1;
} catch (...) { return 0; }

extern "C" void sv_editor_size(SvPlugin* sp, int32_t* w, int32_t* h) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    *w = p->editor_w;
    *h = p->editor_h;
} catch (...) {}

extern "C" void sv_move_editor(SvPlugin* sp, int32_t x, int32_t y) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (p->window && p->embedded)
        x11_move(p->window, x, y);
} catch (...) {}

extern "C" void sv_close_editor(SvPlugin* sp) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (p->view) {
        p->view->removed();
        p->view->setFrame(nullptr);
        p->view = nullptr;
    }
    p->frame = nullptr;
    x11_destroy(p->window);
    p->window = 0;
    p->embedded = false;
} catch (...) {}

// The plug-in's host process has died: every call into it would wait forever, so idle
// housekeeping forgets it and its editor's run loop handlers. Nothing is released.
extern "C" void sv_abandon(SvPlugin* sp) try {
    auto p = reinterpret_cast<Plugin*>(sp);
    if (!p)
        return;
    plugins().erase(p);
    // The cached module still talks to the dead host; the next open loads it afresh. The
    // abandoned plug-in keeps its reference, so the old module is never unloaded.
    for (auto it = modules().begin(); it != modules().end(); ++it)
        if (it->second == p->module) {
            modules().erase(it);
            break;
        }
    if (p->frame) {
        for (auto h : p->frame->own_fds)
            p->frame->unregisterEventHandler(h);
        for (auto h : p->frame->own_timers)
            p->frame->unregisterTimer(h);
        p->frame->own_fds.clear();
        p->frame->own_timers.clear();
    }
    // A floating editor's window is ours; with the plug-in gone nothing else would close it.
    if (p->window && !p->embedded) {
        x11_destroy(p->window);
        p->window = 0;
    }
} catch (...) {}

extern "C" int32_t sv_take_dirty(SvPlugin* sp) try {
    return reinterpret_cast<Plugin*>(sp)->dirty.exchange(false);
} catch (...) { return 0; }

extern "C" unsigned long sv_editor_window(SvPlugin* sp) try {
    return reinterpret_cast<Plugin*>(sp)->window;
} catch (...) { return 0; }

extern "C" int32_t sv_editor_open(SvPlugin* sp) try {
    return reinterpret_cast<Plugin*>(sp)->view != nullptr;
} catch (...) { return 0; }

static void window_closed(unsigned long w) {
    for (auto p : plugins())
        if (p->window == w) {
            sv_close_editor(reinterpret_cast<SvPlugin*>(p));
            return;
        }
}

extern "C" void sv_idle(void) try {
    run_loop().run();
    x11_pump(window_closed);
    for (auto p : plugins()) {
        ParamID id;
        ParamValue v;
        int32 offset;
        while (p->from_proc.getNextChange(id, v, offset))
            if (p->controller)
                p->controller->setParamNormalized(id, v);
    }
} catch (...) {}
