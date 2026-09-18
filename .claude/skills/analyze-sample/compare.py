import sys, numpy as np, soundfile as sf, librosa, warnings; warnings.filterwarnings('ignore')
from scipy.signal import butter, sosfiltfilt, correlate, find_peaks
BANDS={'sub':(20,70),'body':(70,160),'lowmid':(160,400),'mid':(400,2000),'hi':(2000,8000),'air':(8000,16000)}
def rep(path,name,off=0.04):
    x,sr=sf.read(path); m=x.mean(1); bar=4*60/123
    sig={n:sosfiltfilt(butter(4,[lo,hi],'bp',fs=sr,output='sos'),m) for n,(lo,hi) in BANDS.items()}
    tot=sum((y**2).mean() for y in sig.values())
    print(f'== {name}  rms {20*np.log10(np.sqrt((m**2).mean())):.1f} dB')
    print('   share %:', ' '.join(f'{n}={100*(y**2).mean()/tot:5.1f}' for n,y in sig.items()))
    print('   LR corr:', end=' ')
    for n,(lo,hi) in BANDS.items():
        L=sosfiltfilt(butter(4,[lo,hi],'bp',fs=sr,output='sos'),x[:,0]); R=sosfiltfilt(butter(4,[lo,hi],'bp',fs=sr,output='sos'),x[:,1])
        print(f'{n}={np.corrcoef(L,R)[0,1]:+.2f}',end=' ')
    print()
    # pulse depth per band over a bar (32 steps)
    for n in ('body','mid','hi'):
        y=sig[n]; steps=32; acc=np.zeros(steps)
        for b in range(1,14):
            for j in range(steps):
                a=int((off+b*bar+j*bar/steps)*sr); acc[j]+=(y[a:a+int(bar/steps*sr)]**2).mean()
        p=10*np.log10(acc); p-=p.max(); print(f'   {n:6s} bar pulse:'+' '.join(f'{v:4.0f}' for v in p))
    # harmonics of the drone at 3 s
    seg=m[int(3*sr):int(4*sr)]; N=1<<17
    X=np.abs(np.fft.rfft(seg*np.hanning(len(seg)),N)); f=np.fft.rfftfreq(N,1/sr); b=(f>30)&(f<200); f0=f[b][np.argmax(X[b])]
    h=[20*np.log10(X[(f>k*f0*0.985)&(f<k*f0*1.015)].max()/X[b].max()) for k in range(1,13)]
    print(f'   f0 {f0:.1f} Hz ({librosa.hz_to_note(f0)}) harmonics:'+' '.join(f'{v:5.1f}' for v in h))
for p,n in zip(sys.argv[1::2],sys.argv[2::2]): rep(p,n)
