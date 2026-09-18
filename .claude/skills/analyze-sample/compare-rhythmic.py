import sys, numpy as np, soundfile as sf, warnings; warnings.filterwarnings('ignore')
from scipy.signal import butter, sosfiltfilt, find_peaks
BANDS={'sub':(20,70),'body':(70,160),'lowmid':(160,400),'mid':(400,2000),'hi':(2000,8000),'air':(8000,16000)}
def grid(m,sr):
    hf=np.convolve(np.abs(sosfiltfilt(butter(4,[3000,12000],'bp',fs=sr,output='sos'),m)),np.ones(96)/96,'same')
    d=np.diff(hf,prepend=0); pk,_=find_peaks(d,distance=int(0.4*sr),height=np.percentile(d,99)*0.4)
    t=pk/sr; k=np.round((t-t[0])/np.median(np.diff(t))); a,b=np.polyfit(k,t,1); return a,b
def report(path,name,fixed=None):
    x,sr=sf.read(path); m=x.mean(1); side=(x[:,0]-x[:,1])/2
    beat,t0=(fixed if fixed else grid(m,sr)); bar=4*beat; st=beat/4
    sig={n:sosfiltfilt(butter(4,[lo,hi],'bp',fs=sr,output='sos'),m) for n,(lo,hi) in BANDS.items()}
    tot=sum((y**2).mean() for y in sig.values())
    print(f'== {name}: {60/beat:.2f} BPM, first kick {t0:.3f}s, rms {20*np.log10(np.sqrt((m**2).mean())):.1f} dB')
    print('   band share %:', ' '.join(f'{n}={100*(y**2).mean()/tot:4.1f}' for n,y in sig.items()))
    print('   side/mid dB :', ' '.join(f'{n}={10*np.log10(((sosfiltfilt(butter(4,list(BANDS[n]),"bp",fs=sr,output="sos"),side))**2).mean()/(y**2).mean()):5.1f}' for n,y in sig.items()))
    nb=int((len(m)/sr-t0)/bar)-1
    for n,y in sig.items():
        e=np.zeros(4)
        for i in range(1,nb):
            for k in range(16):
                a=int((t0+i*bar+k*st)*sr); e[k%4]+=(y[a:a+int((st-0.015)*sr)]**2).mean()
        e=10*np.log10(e/e.max()); print(f'   {n:6s} by 16th (0=kick):'+' '.join(f'{v:6.1f}' for v in e))
    # beat profile in 20 ms columns
    for n in ('sub','body','mid','air'):
        y=sig[n]; steps=int(beat/0.02); acc=np.zeros(steps)
        for i in range(2,min(60,int((len(m)/sr-t0)/beat)-1)):
            s=int((t0+i*beat)*sr)
            for j in range(steps): acc[j]+=(y[s+int(j*0.02*sr):s+int((j+1)*0.02*sr)]**2).mean()
        p=10*np.log10(acc); p-=p.max(); print(f'   {n:6s} over a beat:'+' '.join(f'{v:5.0f}' for v in p[::2]))
for p,n in zip(sys.argv[1::3],sys.argv[2::3]):
    pass
args=sys.argv[1:]
while args:
    p,n,f=args[0],args[1],args[2]; args=args[3:]
    report(p,n,None if f=='auto' else (60/123.0,float(f)))
