/* Throwaway ENCODER-ONLY wrapper, used solely to validate the decoder via a
 * round-trip in tests. Not shipped / not loaded by the app. */
#include "source.h"
#include "acelp_state_bridge.h"
#include <stdint.h>
#include <string.h>

#define L_FRAME 240

extern void Init_Pre_Process(void);
extern void Pre_Process(Word16 signal[], Word16 lg);
extern void Init_Coder_Tetra(void);
extern void Coder_Tetra(Word16 ana[], Word16 synth[]);
extern void Prm2bits_Tetra(Word16 prm[], Word16 bits[]);
extern void Post_Process(Word16 signal[], Word16 lg);
extern Word16 *new_speech;

static int g_init = 0;

#if defined(_WIN32)
#define ENC_EXPORT __declspec(dllexport)
#else
#define ENC_EXPORT
#endif

ENC_EXPORT void tetra_enc_init(void) {
    Init_Pre_Process();
    Init_Coder_Tetra();
    g_init = 1;
}

ENC_EXPORT void tetra_enc_encode(const int16_t *pcm240, uint8_t *bits137) {
    if (!g_init) tetra_enc_init();
    memcpy(new_speech, pcm240, L_FRAME * sizeof(Word16));
    Pre_Process(new_speech, (Word16)L_FRAME);
    Word16 ana[23];
    Word16 syn[L_FRAME];
    Coder_Tetra(ana, syn);
    Post_Process(syn, (Word16)L_FRAME);
    Word16 serial[138];
    Prm2bits_Tetra(ana, serial);
    for (int i = 0; i < 137; i++) bits137[i] = (uint8_t)(serial[i + 1] & 0x01);
}
