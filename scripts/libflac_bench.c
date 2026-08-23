// In-process FLAC decode timing against the system libFLAC build.
//
// Mirrors examples/codec_bench.rs methodology: the compressed file is read
// into memory once, every run decodes through libFLAC's stream API, samples
// fold into a checksum sink so nothing is optimized away, and the report is
// a median over timed runs after one warm-up run.
//
// Usage:
//
//   libflac_bench FILE.flac [RUNS]

#include <FLAC/stream_decoder.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

typedef struct {
    const uint8_t *data;
    size_t len;
    size_t pos;
    uint64_t sink;
} BenchState;

static double now_seconds(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static FLAC__StreamDecoderReadStatus read_cb(
    const FLAC__StreamDecoder *decoder,
    FLAC__byte buffer[],
    size_t *bytes,
    void *client_data
) {
    (void)decoder;
    BenchState *state = client_data;
    size_t remaining = state->len - state->pos;
    if (*bytes == 0) {
        return FLAC__STREAM_DECODER_READ_STATUS_ABORT;
    }
    if (remaining == 0) {
        *bytes = 0;
        return FLAC__STREAM_DECODER_READ_STATUS_END_OF_STREAM;
    }
    size_t take = remaining < *bytes ? remaining : *bytes;
    memcpy(buffer, state->data + state->pos, take);
    state->pos += take;
    *bytes = take;
    return FLAC__STREAM_DECODER_READ_STATUS_CONTINUE;
}

static FLAC__StreamDecoderWriteStatus write_cb(
    const FLAC__StreamDecoder *decoder,
    const FLAC__Frame *frame,
    const FLAC__int32 *const buffer[],
    void *client_data
) {
    (void)decoder;
    BenchState *state = client_data;
    uint64_t sink = state->sink;
    for (unsigned ch = 0; ch < frame->header.channels; ch++) {
        const FLAC__int32 *samples = buffer[ch];
        for (unsigned i = 0; i < frame->header.blocksize; i++) {
            sink ^= (uint64_t)(uint32_t)samples[i];
        }
    }
    state->sink = sink;
    return FLAC__STREAM_DECODER_WRITE_STATUS_CONTINUE;
}

static void error_cb(
    const FLAC__StreamDecoder *decoder,
    FLAC__StreamDecoderErrorStatus status,
    void *client_data
) {
    (void)decoder;
    (void)client_data;
    fprintf(stderr, "libflac_bench: decoder error: %s\n",
            FLAC__StreamDecoderErrorStatusString[status]);
}

static int compare_double(const void *a, const void *b) {
    double x = *(const double *)a;
    double y = *(const double *)b;
    return x < y ? -1 : x > y ? 1 : 0;
}

static int run_once(BenchState *state, double *elapsed) {
    FLAC__StreamDecoder *decoder = FLAC__stream_decoder_new();
    if (!decoder) {
        fprintf(stderr, "libflac_bench: cannot create decoder\n");
        return 1;
    }
    state->pos = 0;
    state->sink = 0;
    FLAC__stream_decoder_init_stream(
        decoder, read_cb, NULL, NULL, NULL, NULL, write_cb, NULL, error_cb,
        state);

    double start = now_seconds();
    if (!FLAC__stream_decoder_process_until_end_of_stream(decoder)) {
        fprintf(stderr, "libflac_bench: decode failed\n");
        return 1;
    }
    *elapsed = now_seconds() - start;

    FLAC__stream_decoder_delete(decoder);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: libflac_bench FILE.flac [RUNS]\n");
        return 2;
    }
    int runs = argc >= 3 ? atoi(argv[2]) : 5;
    if (runs < 1) {
        runs = 1;
    }

    FILE *file = fopen(argv[1], "rb");
    if (!file) {
        perror(argv[1]);
        return 1;
    }
    fseek(file, 0, SEEK_END);
    long size = ftell(file);
    fseek(file, 0, SEEK_SET);
    uint8_t *data = malloc((size_t)size);
    if (!data || fread(data, 1, (size_t)size, file) != (size_t)size) {
        fprintf(stderr, "libflac_bench: short read\n");
        return 1;
    }
    fclose(file);

    BenchState state = {data, (size_t)size, 0, 0};

    double warmup;
    if (run_once(&state, &warmup)) {
        return 1;
    }
    double *timings = calloc((size_t)runs, sizeof(double));
    for (int i = 0; i < runs; i++) {
        if (run_once(&state, &timings[i])) {
            return 1;
        }
    }
    qsort(timings, (size_t)runs, sizeof(double), compare_double);
    double median = runs % 2 == 1
        ? timings[runs / 2]
        : (timings[runs / 2 - 1] + timings[runs / 2]) / 2.0;

    printf("decode: median %.4f s, min %.4f s over %d runs (warm-up %.4f s)\n",
           median, timings[0], runs, warmup);
    printf("checksum sink: %llu\n", (unsigned long long)state.sink);

    free(timings);
    free(data);
    return 0;
}
