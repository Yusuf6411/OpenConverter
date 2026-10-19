/* Decodes BC6H blocks with bcdec (public domain, github.com/iOrange/bcdec) to half-float
 * bit patterns. Dev-time only; make_bc6h_fixtures.py builds and runs it.
 *   bc6h_oracle <0|1 signed> < blocks > halves   (per block: 16 texels x RGB, u16 LE)
 */
#define BCDEC_IMPLEMENTATION
#include "bcdec.h"
#include <stdio.h>
#include <stdint.h>

int main(int argc, char **argv) {
    int is_signed = argc > 1 && argv[1][0] == '1';
    unsigned char blk[16];
    while (fread(blk, 1, 16, stdin) == 16) {
        uint16_t out[16 * 3];
        bcdec_bc6h_half(blk, out, 4 * 3, is_signed);
        fwrite(out, 2, 16 * 3, stdout);
    }
    return 0;
}
