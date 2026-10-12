# Writes local/assets/science-big/: large synthetic FITS, XISF and film files for timing
# Slerp's science readers.  Needs numpy, astropy, python-xisf and OpenImageIO.
# Seeded, so every run writes the same files.
import numpy as np, struct, subprocess, sys, os
from astropy.io import fits
out=os.path.join(os.path.dirname(os.path.abspath(__file__)),'..','local','assets','science-big')
rng=np.random.default_rng(1)
def sky(h,w,dtype=np.float32):
    y,x=np.mgrid[0:h:1.0,0:w:1.0]
    a=1000+300*np.sin(x/500.0)*np.cos(y/700.0)+rng.normal(0,25,(h,w))
    for _ in range(300):
        cx,cy=rng.uniform(0,w),rng.uniform(0,h); r=rng.uniform(2,8); amp=rng.uniform(2000,40000)
        x0,x1=int(max(cx-40,0)),int(min(cx+40,w)); y0,y1=int(max(cy-40,0)),int(min(cy+40,h))
        yy,xx=np.mgrid[y0:y1,x0:x1]
        a[y0:y1,x0:x1]+=amp*np.exp(-((xx-cx)**2+(yy-cy)**2)/(2*r*r))
    return a.astype(dtype)
a=sky(7000,7000)
fits.PrimaryHDU(np.clip(a,0,65535).astype(np.uint16)).writeto(f'{out}/u16_7000.fits',overwrite=True)
print('u16 done')
fits.PrimaryHDU(sky(4000,4000)).writeto(f'{out}/f32_4000.fits',overwrite=True)
i16=(np.clip(a[:4000,:4000],0,32767)).astype(np.int16)
h=fits.CompImageHDU(i16,compression_type='RICE_1',tile_shape=(16,4000))
fits.HDUList([fits.PrimaryHDU(),h]).writeto(f'{out}/rice_4000.fits',overwrite=True)
h=fits.CompImageHDU(sky(4000,4000),compression_type='RICE_1',tile_shape=(16,4000),quantize_level=16.0,quantize_method=1)
fits.HDUList([fits.PrimaryHDU(),h]).writeto(f'{out}/rice_f32_4000.fits',overwrite=True)
h=fits.CompImageHDU(i16,compression_type='HCOMPRESS_1',tile_shape=(64,64),hcomp_scale=0)
fits.HDUList([fits.PrimaryHDU(),h]).writeto(f'{out}/hcomp_4000.fits',overwrite=True)
print('fits done')
# XISF
import xisf
xisf.XISF.write(f'{out}/f32_4000.xisf', sky(4000,4000)[...,None]/65535.0, codec=None)
xisf.XISF.write(f'{out}/f32_4000_zstd.xisf', (sky(4000,4000)[...,None]/65535.0).astype(np.float32), codec='zstd', shuffle=True)
print('xisf done')
# DPX 4K 10-bit and 6K 16-bit via OIIO
import OpenImageIO as oiio
def dpx(name,w,h,bits,ch=3):
    img=np.stack([sky(h,w)/65535.0*2 for _ in range(ch)],axis=-1).astype(np.float32)
    img=np.clip(img,0,1)
    o=oiio.ImageOutput.create(name); spec=oiio.ImageSpec(w,h,ch,oiio.UINT16 if bits>8 else oiio.UINT8); spec.attribute('oiio:BitsPerSample',bits)
    if bits==10: spec.attribute('dpx:Packing','Filled, method A')
    o.open(name,spec); o.write_image(img); o.close()
dpx(f'{out}/dpx_4k_10bit.dpx',4096,3112,10)
dpx(f'{out}/dpx_6k_16bit.dpx',6144,3160,16)
dpx(f'{out}/dpx_4k_12bit_packed.dpx',4096,3112,12)
print('dpx done')
# PAM / farbfeld 4096x4096
w=h=4096
rgba=(np.clip(np.stack([sky(h,w)/65535.0*2]*3+[np.ones((h,w),np.float32)],-1),0,1)*255).astype(np.uint8)
open(f'{out}/rgba_4096.pam','wb').write(f'P7\nWIDTH {w}\nHEIGHT {h}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n'.encode()+rgba.tobytes())
w16=(rgba.astype(np.uint16)*257).astype('>u2')
open(f'{out}/rgba_4096.ff','wb').write(b'farbfeld'+struct.pack('>II',w,h)+w16.tobytes())
print('pam done')
