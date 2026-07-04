import numpy as np, soundfile as sf, onnxruntime as ort, kaldi_native_fbank as knf
enc=ort.InferenceSession("encoder.int8.onnx"); dec=ort.InferenceSession("decoder.int8.onnx"); joi=ort.InferenceSession("joiner.int8.onnx")
toks=[l.rsplit(" ",1)[0] for l in open("tokens.txt",encoding="utf-8").read().splitlines()]
window=65; shift=56; blank=1024
wav,sr=sf.read("test_wavs/0.wav"); wav=(wav if wav.ndim==1 else wav.mean(1)).astype(np.float32)
def feats_of(scale):
    o=knf.FbankOptions(); o.frame_opts.samp_freq=16000; o.frame_opts.dither=0; o.frame_opts.snip_edges=False; o.mel_opts.num_bins=80
    fb=knf.OnlineFbank(o); fb.accept_waveform(16000,(wav*scale).tolist()); fb.input_finished()
    return np.stack([np.array(fb.get_frame(i)) for i in range(fb.num_frames_ready)]).astype(np.float32)
def decode(feats):
    h=np.zeros((1,1,640),np.float32); c=np.zeros((1,1,640),np.float32)
    def rd(tok):
        oo=dec.run(None,{"targets":np.array([[tok]],np.int32),"target_length":np.array([1],np.int32),"states.1":h,"onnx::LSTM_3":c}); return oo[0],oo[2],oo[3]
    dec_out,h,c=rd(blank)
    cache1=np.zeros((1,17,70,512),np.float32); cache2=np.zeros((1,17,512,8),np.float32); clen=np.zeros((1,),np.int64)
    T=feats.shape[0]; off=0; hyp=[]
    while off<T:
        ch=feats[off:off+window]
        if ch.shape[0]<window: ch=np.pad(ch,((0,window-ch.shape[0]),(0,0)))
        eo=enc.run(None,{"audio_signal":ch.T[None,:,:].astype(np.float32),"length":np.array([window],np.int64),"cache_last_channel":cache1,"cache_last_time":cache2,"cache_last_channel_len":clen})
        out,cache1,cache2,clen=eo[0],eo[2],eo[3],eo[4]
        for t in range(out.shape[2]):
            ecol=out[:,:,t:t+1]; sym=0
            while sym<10:
                lo=joi.run(None,{"encoder_outputs":ecol,"decoder_outputs":dec_out})[0].reshape(-1); k=int(lo.argmax())
                if k==blank: break
                hyp.append(k); dec_out,h,c=rd(k); sym+=1
        off+=shift
    return "".join(toks[i] for i in hyp).replace("▁"," ").strip()
for scale in [1.0,32768.0]:
    F=feats_of(scale)
    m=F.mean(0,keepdims=True); s=F.std(0,keepdims=True)+1e-5
    print(f"\nscale={scale} feats mean={F.mean():.2f}")
    print("  RAW :",decode(F)[:80])
    print("  NORM:",decode((F-m)/s)[:80])
