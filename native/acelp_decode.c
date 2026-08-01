/*
 * Decoder-only wrapper around the ETSI EN 300 395-2 TETRA ACELP reference code.
 *
 * Exposes a tiny, stable C ABI that Python (ctypes) can drive:
 *   - create/destroy a per-call decoder context (keeps the codec's internal
 *     history so consecutive frames decode correctly),
 *   - decode one 137-bit ACELP sub-frame -> 240 PCM samples (30 ms @ 8 kHz),
 *     with an explicit BFI (Bad Frame Indicator) input for error concealment.
 *
 * The 137 input bits are expected in codec/STE order (one bit per byte, 0/1) --
 * exactly what MsSpeechFrame.data delivers per §3 of the voice-RX brief.
 */

#include "source.h"
#include "acelp_state_bridge.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define L_FRAME 240
#define DEC_PARAM_SIZE 24 /* bfi + 23 parameters expected by Decod_Tetra */
#define SERIAL_SIZE 138   /* bfi + 137 bits */

/* ETSI reference decoder entry points (sdec_tet.c / sub_sc_d.c / sub_dsp.c ...) */
extern void Init_Decod_Tetra(void);
extern void Bits2prm_Tetra(Word16 bits[], Word16 prm[]);
extern void Decod_Tetra(Word16 parm[], Word16 synth[]);
extern void Post_Process(Word16 signal[], Word16 lg);

struct acelp_decoder_state {
    acelp_sdec_state_t sdec;
    acelp_postproc_state_t postproc;
    acelp_tetraop_state_t tetraop;
    int initialized;
};

typedef struct tetra_dec_ctx {
    struct acelp_decoder_state state;
} tetra_dec_ctx;

#if defined(_WIN32)
#define TDEC_EXPORT __declspec(dllexport)
#else
#define TDEC_EXPORT __attribute__((visibility("default")))
#endif

static void postproc_state_init(acelp_postproc_state_t *state) {
    memset(state, 0, sizeof(*state));
    state->old_a[0] = 4096;
}

static void ensure_dec_init(struct acelp_decoder_state *state) {
    if (!state->initialized) {
        Init_Decod_Tetra();
        acelp_sdec_state_get(&state->sdec);
        postproc_state_init(&state->postproc);
        acelp_tetraop_state_get(&state->tetraop);
        state->initialized = 1;
    }
}

TDEC_EXPORT tetra_dec_ctx *tetra_dec_create(void) {
    return (tetra_dec_ctx *)calloc(1, sizeof(tetra_dec_ctx));
}

TDEC_EXPORT void tetra_dec_destroy(tetra_dec_ctx *ctx) {
    if (ctx) {
        free(ctx);
    }
}

/*
 * Decode one 137-bit sub-frame into 240 int16 PCM samples.
 *   bits137 : 137 bytes, each 0 or 1, in codec/STE order.
 *   bfi     : 0 = good frame (normal decode), non-zero = concealment.
 * Returns 0 on success, negative on bad arguments.
 */
TDEC_EXPORT int tetra_dec_decode(tetra_dec_ctx *ctx, const uint8_t *bits137,
                                 int bfi, int16_t *pcm240) {
    if (!ctx || !bits137 || !pcm240) {
        return -1;
    }

    struct acelp_decoder_state *state = &ctx->state;
    ensure_dec_init(state);

    /* Restore this context's codec history before decoding. */
    acelp_tetraop_state_set(&state->tetraop);
    acelp_postproc_state_set(&state->postproc);
    acelp_sdec_state_set(&state->sdec);

    /* Build serial buffer: BFI + 137 bits, each as a Word16 (0/1). */
    Word16 serial[SERIAL_SIZE];
    serial[0] = (Word16)(bfi ? 1 : 0);
    for (int i = 0; i < 137; i++) {
        serial[i + 1] = (Word16)(bits137[i] & 0x01);
    }

    Word16 parm[DEC_PARAM_SIZE];
    Word16 synth[L_FRAME];

    Bits2prm_Tetra(serial, parm);
    Decod_Tetra(parm, synth);
    Post_Process(synth, (Word16)L_FRAME);

    memcpy(pcm240, synth, L_FRAME * sizeof(int16_t));

    /* Persist updated history back into this context. */
    acelp_sdec_state_get(&state->sdec);
    acelp_postproc_state_get(&state->postproc);
    acelp_tetraop_state_get(&state->tetraop);

    return 0;
}
