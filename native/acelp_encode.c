/*
 * Encoder-only wrapper around the ETSI EN 300 395-2 TETRA ACELP reference code.
 *
 * The exact mirror of acelp_decode.c: exposes a tiny, stable C ABI that Python
 * (ctypes) can drive to turn microphone PCM into the raw ACELP codec bits the
 * MS ships over the air (voice-TX / uplink-audio brief, phase V2 = talk).
 *
 *   - create/destroy a per-call encoder context (keeps the codec's internal
 *     analysis history so consecutive frames encode correctly),
 *   - encode one 240-sample PCM sub-frame (30 ms @ 8 kHz) -> 137 ACELP bits.
 *
 * The 137 output bits are produced in codec/STE order (one bit per byte, 0/1) --
 * exactly the layout MsUplinkSpeech.data expects (and byte-for-byte identical to
 * what MsSpeechFrame.data delivers on RX), per §3 of the voice-TX brief.
 *
 * Re-entrancy: the ETSI reference encoder keeps its analysis state in file-scope
 * globals (old_speech/old_wsp/old_exc/LSP history in scod_tet.c, the pre-process
 * filter memory in sub_dsp.c, and the DSP overflow/carry flags in tetra_op.c).
 * We snapshot those globals into this context after every frame and restore them
 * before the next, so independent encoders (e.g. distinct calls) never clobber
 * one another -- the same trick acelp_decode.c uses for the decoder.
 */

#include "source.h"
#include "acelp_state_bridge.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define L_FRAME 240
#define ANA_SIZE 23      /* 23 ACELP parameters produced by Coder_Tetra */
#define SERIAL_SIZE 138  /* sync/BFI marker + 137 bits */

/* ETSI reference encoder entry points (scod_tet.c / sub_sc_d.c / sub_dsp.c ...) */
extern void Init_Pre_Process(void);
extern void Pre_Process(Word16 signal[], Word16 lg);
extern void Init_Coder_Tetra(void);
extern void Coder_Tetra(Word16 ana[], Word16 synth[]);
extern void Prm2bits_Tetra(Word16 prm[], Word16 bits[]);
extern Word16 *new_speech; /* points into the coder's global old_speech buffer */

struct acelp_encoder_state {
    acelp_scod_state_t scod;
    acelp_preproc_state_t preproc;
    acelp_tetraop_state_t tetraop;
    int initialized;
};

typedef struct tetra_enc_ctx {
    struct acelp_encoder_state state;
} tetra_enc_ctx;

#if defined(_WIN32)
#define TENC_EXPORT __declspec(dllexport)
#else
#define TENC_EXPORT __attribute__((visibility("default")))
#endif

static void ensure_enc_init(struct acelp_encoder_state *state) {
    if (!state->initialized) {
        Init_Pre_Process();
        Init_Coder_Tetra();
        acelp_scod_state_get(&state->scod);
        acelp_preproc_state_get(&state->preproc);
        acelp_tetraop_state_get(&state->tetraop);
        state->initialized = 1;
    }
}

TENC_EXPORT tetra_enc_ctx *tetra_enc_create(void) {
    return (tetra_enc_ctx *)calloc(1, sizeof(tetra_enc_ctx));
}

TENC_EXPORT void tetra_enc_destroy(tetra_enc_ctx *ctx) {
    if (ctx) {
        free(ctx);
    }
}

/*
 * Encode one 240-sample PCM sub-frame into 137 ACELP bits.
 *   pcm240  : 240 int16 samples (30 ms @ 8 kHz), linear PCM.
 *   bits137 : 137 bytes out, each 0 or 1, in codec/STE order.
 * Returns 0 on success, negative on bad arguments.
 */
TENC_EXPORT int tetra_enc_encode(tetra_enc_ctx *ctx, const int16_t *pcm240,
                                 uint8_t *bits137) {
    if (!ctx || !pcm240 || !bits137) {
        return -1;
    }

    struct acelp_encoder_state *state = &ctx->state;
    ensure_enc_init(state);

    /* Restore this context's codec history before analysing. */
    acelp_tetraop_state_set(&state->tetraop);
    acelp_preproc_state_set(&state->preproc);
    acelp_scod_state_set(&state->scod);

    /* Feed the 240 new samples into the coder's global input window and run the
     * high-pass pre-process, then the ACELP analysis (scoder.c order). */
    memcpy(new_speech, pcm240, L_FRAME * sizeof(Word16));
    Pre_Process(new_speech, (Word16)L_FRAME);

    Word16 ana[ANA_SIZE];
    Word16 synth[L_FRAME];
    Word16 serial[SERIAL_SIZE];

    Coder_Tetra(ana, synth);
    Prm2bits_Tetra(ana, serial);

    /* serial[0] is the sync/BFI marker; the 137 payload bits are serial[1..137]. */
    for (int i = 0; i < 137; i++) {
        bits137[i] = (uint8_t)(serial[i + 1] & 0x01);
    }

    /* Persist updated history back into this context. */
    acelp_scod_state_get(&state->scod);
    acelp_preproc_state_get(&state->preproc);
    acelp_tetraop_state_get(&state->tetraop);

    return 0;
}
