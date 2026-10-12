/*
 * A small entry point into jxrlib (Microsoft's JPEG XR reference decoder,
 * BSD-2-Clause, vendored in vendor/jxrlib) that decodes a file in memory
 * to one of a few simple pixel layouts, optionally at a reduced size, which
 * the Rust side then widens to RGBA.
 *
 * jxrlib trusts much of what a file says, so the Rust side calls this only
 * in a child process with limits.  Everything here still checks what it can:
 * the pixel buffer is bounded by `max_bytes` before it is allocated.
 */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "jxrgluelib/JXRGlue.h"

/* Layouts the Rust side understands; keep in step with src/lib.rs. */
enum {
    LAYOUT_GRAY8 = 0,
    LAYOUT_RGB24 = 1,
    LAYOUT_BGR24 = 2,
    LAYOUT_BGR32 = 3,
    LAYOUT_RGBA32 = 4,
    LAYOUT_BGRA32 = 5,
    LAYOUT_PRGBA32 = 6,
    LAYOUT_PBGRA32 = 7,
    LAYOUT_GRAYF32 = 8,
    LAYOUT_RGBF32 = 9,
    LAYOUT_RGBXF32 = 10,
    LAYOUT_RGBAF32 = 11,
    LAYOUT_PRGBAF32 = 12,
    LAYOUT_GRAYF16 = 13,
    LAYOUT_RGBF16 = 14,
    LAYOUT_RGBXF16 = 15,
    LAYOUT_RGBAF16 = 16
};

typedef struct {
    int32_t status;      /* 0 on success, else a jxrlib error code */
    int32_t width;       /* of the decoded (possibly reduced) picture */
    int32_t height;
    int32_t layout;
    int32_t stride;      /* bytes from one row to the next in `pixels` */
    int32_t alpha;       /* the file has an alpha channel */
    int32_t full_width;  /* of the file's picture */
    int32_t full_height;
    float dpi_x;
    float dpi_y;
    char native[40];     /* the file's pixel format by name */
    char message[64];    /* why decoding failed, when it did */
} SlerpJxrOut;

typedef struct {
    const PKPixelFormatGUID *native;
    const char *name;
    const PKPixelFormatGUID *target;
    int layout;
} Route;

/* Which format jxrlib's converter should produce for each file format.
 * 8-bit formats stay as they are; 16-bit integer and packed ones drop to 8
 * bits; half, fixed-point and float ones become 32-bit float. */
static const Route routes[] = {
    {&GUID_PKPixelFormatBlackWhite, "1-bit black and white", &GUID_PKPixelFormat8bppGray, LAYOUT_GRAY8},
    {&GUID_PKPixelFormat8bppGray, "8-bit grey", &GUID_PKPixelFormat8bppGray, LAYOUT_GRAY8},
    {&GUID_PKPixelFormat16bppGray, "16-bit grey", &GUID_PKPixelFormat8bppGray, LAYOUT_GRAY8},
    {&GUID_PKPixelFormat16bppGrayFixedPoint, "16-bit fixed-point grey", &GUID_PKPixelFormat32bppGrayFloat, LAYOUT_GRAYF32},
    {&GUID_PKPixelFormat16bppGrayHalf, "half-float grey", &GUID_PKPixelFormat16bppGrayHalf, LAYOUT_GRAYF16},
    {&GUID_PKPixelFormat32bppGrayFixedPoint, "32-bit fixed-point grey", &GUID_PKPixelFormat32bppGrayFloat, LAYOUT_GRAYF32},
    {&GUID_PKPixelFormat32bppGrayFloat, "32-bit float grey", &GUID_PKPixelFormat32bppGrayFloat, LAYOUT_GRAYF32},
    {&GUID_PKPixelFormat16bppRGB555, "RGB 5-5-5", &GUID_PKPixelFormat24bppRGB, LAYOUT_RGB24},
    {&GUID_PKPixelFormat16bppRGB565, "RGB 5-6-5", &GUID_PKPixelFormat24bppRGB, LAYOUT_RGB24},
    {&GUID_PKPixelFormat24bppRGB, "8-bit RGB", &GUID_PKPixelFormat24bppRGB, LAYOUT_RGB24},
    {&GUID_PKPixelFormat24bppBGR, "8-bit BGR", &GUID_PKPixelFormat24bppBGR, LAYOUT_BGR24},
    {&GUID_PKPixelFormat32bppBGR, "8-bit BGR, padded", &GUID_PKPixelFormat32bppBGR, LAYOUT_BGR32},
    {&GUID_PKPixelFormat32bppRGB, "8-bit RGB, padded", &GUID_PKPixelFormat24bppRGB, LAYOUT_RGB24},
    {&GUID_PKPixelFormat32bppBGRA, "8-bit BGRA", &GUID_PKPixelFormat32bppBGRA, LAYOUT_BGRA32},
    {&GUID_PKPixelFormat32bppRGBA, "8-bit RGBA", &GUID_PKPixelFormat32bppRGBA, LAYOUT_RGBA32},
    {&GUID_PKPixelFormat32bppPBGRA, "8-bit premultiplied BGRA", &GUID_PKPixelFormat32bppPBGRA, LAYOUT_PBGRA32},
    {&GUID_PKPixelFormat32bppPRGBA, "8-bit premultiplied RGBA", &GUID_PKPixelFormat32bppPRGBA, LAYOUT_PRGBA32},
    {&GUID_PKPixelFormat32bppRGB101010, "RGB 10-10-10", &GUID_PKPixelFormat24bppRGB, LAYOUT_RGB24},
    {&GUID_PKPixelFormat48bppRGB, "16-bit RGB", &GUID_PKPixelFormat24bppRGB, LAYOUT_RGB24},
    {&GUID_PKPixelFormat48bppRGBFixedPoint, "16-bit fixed-point RGB", &GUID_PKPixelFormat96bppRGBFloat, LAYOUT_RGBF32},
    {&GUID_PKPixelFormat48bppRGBHalf, "half-float RGB", &GUID_PKPixelFormat48bppRGBHalf, LAYOUT_RGBF16},
    {&GUID_PKPixelFormat64bppRGBA, "16-bit RGBA", &GUID_PKPixelFormat32bppRGBA, LAYOUT_RGBA32},
    {&GUID_PKPixelFormat64bppRGBFixedPoint, "16-bit fixed-point RGB, padded", &GUID_PKPixelFormat96bppRGBFloat, LAYOUT_RGBF32},
    {&GUID_PKPixelFormat64bppRGBHalf, "half-float RGB, padded", &GUID_PKPixelFormat64bppRGBHalf, LAYOUT_RGBXF16},
    {&GUID_PKPixelFormat64bppRGBAFixedPoint, "16-bit fixed-point RGBA", &GUID_PKPixelFormat128bppRGBAFloat, LAYOUT_RGBAF32},
    {&GUID_PKPixelFormat64bppRGBAHalf, "half-float RGBA", &GUID_PKPixelFormat64bppRGBAHalf, LAYOUT_RGBAF16},
    {&GUID_PKPixelFormat96bppRGBFixedPoint, "32-bit fixed-point RGB", &GUID_PKPixelFormat24bppRGB, LAYOUT_RGB24},
    {&GUID_PKPixelFormat96bppRGBFloat, "32-bit float RGB", &GUID_PKPixelFormat96bppRGBFloat, LAYOUT_RGBF32},
    {&GUID_PKPixelFormat128bppRGBFixedPoint, "32-bit fixed-point RGB, padded", &GUID_PKPixelFormat96bppRGBFloat, LAYOUT_RGBF32},
    {&GUID_PKPixelFormat128bppRGBFloat, "32-bit float RGB, padded", &GUID_PKPixelFormat128bppRGBFloat, LAYOUT_RGBXF32},
    {&GUID_PKPixelFormat128bppRGBAFixedPoint, "32-bit fixed-point RGBA", &GUID_PKPixelFormat32bppRGBA, LAYOUT_RGBA32},
    {&GUID_PKPixelFormat128bppRGBAFloat, "32-bit float RGBA", &GUID_PKPixelFormat128bppRGBAFloat, LAYOUT_RGBAF32},
    {&GUID_PKPixelFormat128bppPRGBAFloat, "32-bit float premultiplied RGBA", &GUID_PKPixelFormat128bppPRGBAFloat, LAYOUT_PRGBAF32},
    {&GUID_PKPixelFormat32bppRGBE, "RGBE", &GUID_PKPixelFormat96bppRGBFloat, LAYOUT_RGBF32},
};

static size_t bits_of_layout(int layout)
{
    switch (layout) {
    case LAYOUT_GRAY8: return 8;
    case LAYOUT_GRAYF16: return 16;
    case LAYOUT_RGBF16: return 48;
    case LAYOUT_RGBXF16:
    case LAYOUT_RGBAF16: return 64;
    case LAYOUT_RGB24:
    case LAYOUT_BGR24: return 24;
    case LAYOUT_BGR32:
    case LAYOUT_RGBA32:
    case LAYOUT_BGRA32:
    case LAYOUT_PRGBA32:
    case LAYOUT_PBGRA32:
    case LAYOUT_GRAYF32: return 32;
    case LAYOUT_RGBF32: return 96;
    default: return 128;
    }
}

static void fail(SlerpJxrOut *out, int status, const char *message)
{
    out->status = status ? status : -1;
    strncpy(out->message, message, sizeof(out->message) - 1);
    out->message[sizeof(out->message) - 1] = 0;
}

/*
 * Decodes `data` (a JPEG XR file) into a malloc'd buffer in `*pixels`.  With
 * a `min_size` the picture is decoded at 1/2^k of its size for the largest
 * k (at most 4) that leaves its longer side at least that long: JPEG XR can
 * skip the finest detail, which makes thumbnails of big pictures cheap.  The
 * limit is jxrlib's: it divides 16 pixel rows by the reduction, so 1/32 and
 * smaller crash it with a division by zero.  Returns 0 on success; otherwise
 * `out->status` and `out->message` say why.  The buffer holds `out->stride * out->height`
 * bytes; free it with slerp_jxr_free.
 */
int slerp_jxr_decode(const uint8_t *data, size_t len, uint32_t min_size, uint64_t max_bytes, SlerpJxrOut *out,
                     uint8_t **pixels)
{
    uint32_t shift = 0;
    ERR err = WMP_errSuccess;
    PKCodecFactory *factory = NULL;
    PKImageDecode *decoder = NULL;
    PKFormatConverter *converter = NULL;
    struct WMPStream *stream = NULL;
    PKPixelInfo info;
    const Route *route = NULL;
    uint8_t *buffer = NULL;
    size_t i;

    memset(out, 0, sizeof(*out));
    *pixels = NULL;
    if (len < 8) {
        fail(out, WMP_errUnsupportedFormat, "too short for a JPEG XR file");
        return out->status;
    }

    Call(PKCreateCodecFactory(&factory, WMP_SDK_VERSION));
    Call(CreateWS_Memory(&stream, (void *)data, len));
    Call(PKCodecFactory_CreateCodec(&IID_PKImageWmpDecode, (void **)&decoder));
    Call(decoder->Initialize(decoder, stream));
    /* The decoder now owns the stream and frees it with itself. */
    decoder->fStreamOwner = !0;
    stream = NULL;

    {
        I32 w = 0, h = 0;
        Call(decoder->GetSize(decoder, &w, &h));
        out->full_width = w;
        out->full_height = h;
        FailIf(w <= 0 || h <= 0, WMP_errUnsupportedFormat);
        if (min_size > 0) {
            while (shift < 4 && (((uint32_t)(w > h ? w : h)) >> (shift + 1)) >= min_size) shift++;
        }
    }
    {
        Float rx = 0, ry = 0;
        if (decoder->GetResolution(decoder, &rx, &ry) == WMP_errSuccess) {
            out->dpi_x = rx;
            out->dpi_y = ry;
        }
    }

    for (i = 0; i < sizeof(routes) / sizeof(routes[0]); i++) {
        if (IsEqualGUID(&decoder->guidPixFormat, routes[i].native)) {
            route = &routes[i];
            break;
        }
    }
    if (route == NULL) {
        PKPixelInfo probe;
        probe.pGUIDPixFmt = &decoder->guidPixFormat;
        if (PixelFormatLookup(&probe, LOOKUP_FORWARD) == WMP_errSuccess && probe.cfColorFormat == CMYK) {
            fail(out, WMP_errUnsupportedFormat, "CMYK JPEG XR files are not shown");
        } else if (PixelFormatLookup(&probe, LOOKUP_FORWARD) == WMP_errSuccess &&
                   (probe.cfColorFormat == NCOMPONENT)) {
            fail(out, WMP_errUnsupportedFormat, "N-channel JPEG XR files are not shown");
        } else {
            fail(out, WMP_errUnsupportedFormat, "unsupported JPEG XR pixel format");
        }
        goto Cleanup;
    }
    strncpy(out->native, route->name, sizeof(out->native) - 1);

    /* Configure the decoder as jxrlib's own decoder program does. */
    info.pGUIDPixFmt = &decoder->guidPixFormat;
    Call(PixelFormatLookup(&info, LOOKUP_FORWARD));
    out->alpha = (info.grBit & PK_pixfmtHasAlpha) ? 1 : 0;
    decoder->WMP.wmiSCP.bfBitstreamFormat = FREQUENCY;
    decoder->WMP.wmiSCP.uAlphaMode = out->alpha ? 2 : 0;
    decoder->WMP.wmiSCP.sbSubband = SB_ALL;
    decoder->WMP.bIgnoreOverlap = FALSE;
    decoder->WMP.wmiI.cfColorFormat = info.cfColorFormat;
    decoder->WMP.wmiI.bdBitDepth = info.bdBitDepth;
    decoder->WMP.wmiI.cBitsPerUnit = info.cbitUnit;
    decoder->WMP.wmiI.cThumbnailWidth = decoder->WMP.wmiI.cWidth;
    decoder->WMP.wmiI.cThumbnailHeight = decoder->WMP.wmiI.cHeight;
    decoder->WMP.wmiI.bSkipFlexbits = FALSE;
    if (shift > 0) {
        size_t size = (size_t)1 << (shift > 24 ? 24 : shift);
        decoder->WMP.wmiI.cThumbnailWidth = (decoder->WMP.wmiI.cWidth + size - 1) / size;
        decoder->WMP.wmiI.cThumbnailHeight = (decoder->WMP.wmiI.cHeight + size - 1) / size;
        /* Reduced-size decoding doesn't do 4:2:0 and 4:2:2 chroma. */
        if (decoder->WMP.wmiI.cfColorFormat == YUV_420 || decoder->WMP.wmiI.cfColorFormat == YUV_422)
            decoder->WMP.wmiI.cfColorFormat = YUV_444;
    }
    decoder->WMP.wmiI.cROILeftX = 0;
    decoder->WMP.wmiI.cROITopY = 0;
    decoder->WMP.wmiI.cROIWidth = decoder->WMP.wmiI.cThumbnailWidth;
    decoder->WMP.wmiI.cROIHeight = decoder->WMP.wmiI.cThumbnailHeight;
    decoder->WMP.wmiI.oOrientation = O_NONE;
    decoder->WMP.wmiI.cPostProcStrength = 0;
    decoder->WMP.wmiSCP.bVerbose = FALSE;

    {
        size_t width = decoder->WMP.wmiI.cROIWidth;
        size_t height = decoder->WMP.wmiI.cROIHeight;
        size_t bits = bits_of_layout(route->layout);
        size_t native_bits = info.cbitUnit;
        size_t stride, total;
        PKRect rect;
        FailIf(width == 0 || height == 0 || width > (1u << 20) || height > (1u << 20), WMP_errUnsupportedFormat);
        if (native_bits > bits) bits = native_bits;
        stride = (((bits * width) + 7) / 8 + 15) & ~(size_t)15;
        total = stride * height;
        if (height != 0 && total / height != stride) {
            fail(out, WMP_errOutOfMemory, "picture too large");
            goto Cleanup;
        }
        if (total > max_bytes) {
            fail(out, WMP_errOutOfMemory, "picture too large for the decode limit");
            goto Cleanup;
        }
        /* Some converters read a little past the last row's pixels. */
        buffer = (uint8_t *)calloc(total + 64, 1);
        if (buffer == NULL) {
            fail(out, WMP_errOutOfMemory, "out of memory");
            goto Cleanup;
        }
        Call(PKCodecFactory_CreateFormatConverter(&converter));
        Call(converter->Initialize(converter, decoder, NULL, *route->target));
        rect.X = 0;
        rect.Y = 0;
        rect.Width = (I32)width;
        rect.Height = (I32)height;
        Call(converter->Copy(converter, &rect, buffer, (U32)stride));
        out->width = (int32_t)width;
        out->height = (int32_t)height;
        out->stride = (int32_t)stride;
        out->layout = route->layout;
        *pixels = buffer;
        buffer = NULL;
    }

Cleanup:
    if (err != WMP_errSuccess && out->message[0] == 0) {
        switch (err) {
        case WMP_errOutOfMemory: fail(out, err, "out of memory"); break;
        case WMP_errUnsupportedFormat: fail(out, err, "unsupported JPEG XR file"); break;
        case WMP_errBufferOverflow: fail(out, err, "damaged JPEG XR file (buffer overflow caught)"); break;
        case WMP_errInvalidParameter: fail(out, err, "damaged JPEG XR file (bad parameter)"); break;
        default: fail(out, err, "damaged or unsupported JPEG XR file"); break;
        }
    }
    free(buffer);
    if (converter) converter->Release(&converter);
    if (decoder) decoder->Release(&decoder);
    if (stream) stream->Close(&stream);
    if (factory) PKCreateCodecFactory_Release(&factory);
    return out->status;
}

void slerp_jxr_free(uint8_t *pixels)
{
    free(pixels);
}
