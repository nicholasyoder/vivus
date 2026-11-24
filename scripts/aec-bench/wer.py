"""Word error rate of listen_file output against scene.py's truth.txt, over the
whole run (so it doesn't depend on how the VAD split utterances).
Usage: wer.py truth.txt hyp.txt"""
import sys, re
def norm(s): return re.sub(r"[^a-z0-9' ]", " ", s.lower()).split()
truth = [norm(l) for l in open(sys.argv[1]) if l.strip()]
hyp_line = open(sys.argv[2]).read()
# listen_file prints: file: SpeechPossible SpeechStarted "text" ...
hyp = norm(" ".join(re.findall(r'"([^"]*)"', hyp_line)))
ref = [w for t in truth for w in t]
# word edit distance over the whole run (segmentation-independent)
d = list(range(len(hyp) + 1))
for i, r in enumerate(ref, 1):
    prev, d[0] = d[0], i
    for j, h in enumerate(hyp, 1):
        cur = min(d[j] + 1, d[j-1] + 1, prev + (r != h)); prev, d[j] = d[j], cur
print(f"WER {100*d[-1]/len(ref):5.1f}%  ({d[-1]} errors / {len(ref)} words)")
