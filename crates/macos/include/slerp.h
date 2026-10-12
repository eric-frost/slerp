/*
 * The C interface of libslerp_macos (crates/macos), which Slerp's
 * Quick Look thumbnail extension calls.  src/lib.rs explains each function;
 * a test there checks that the status values below match it.
 */
#ifndef SLERP_H
#define SLERP_H

#include <stddef.h>
#include <stdint.h>

/* 8-bit RGBA, straight alpha, sRGB, top row first: width * height * 4
   bytes.  Free with slerp_free. */
typedef struct SlerpImage {
    uint8_t *rgba;
    uint32_t width;
    uint32_t height;
} SlerpImage;

/* The thumbnail is in out. */
#define SLERP_OK 0
/* Not a file Slerp makes thumbnails of; let the system draw it. */
#define SLERP_NOT_MINE 1
/* The file couldn't be read, decoded or drawn; the message says why. */
#define SLERP_FAILED 2
/* A bug: Slerp panicked, and recovered. */
#define SLERP_PANICKED 3
/* A null pointer or an empty path. */
#define SLERP_BAD_ARGUMENT 4

/* Makes the thumbnail of the file at path, no larger than max_size pixels
   on its longer side.  Safe to call from several threads at once.  On
   failure, out is empty and, when message isn't NULL, a NUL-terminated
   explanation of at most message_capacity bytes is written there. */
int32_t slerp_thumbnail(const char *path, uint32_t max_size, SlerpImage *out, char *message,
                            size_t message_capacity);

/* Frees a thumbnail and empties *image.  NULL and empty images are fine. */
void slerp_free(SlerpImage *image);

#endif
