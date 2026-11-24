"""Word error rate of listen_file output against scene.py's truth.txt, over the
whole run (so it doesn't depend on how the VAD split utterances).
Usage: wer.py truth.txt hyp.txt"""
import sys, re
ONES = "zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen".split()
TENS = "_ _ twenty thirty forty fifty sixty seventy eighty ninety".split()
def say(n):
    n = int(n)
    if n < 20: return ONES[n]
    if n < 100: return TENS[n // 10] + ("" if n % 10 == 0 else " " + ONES[n % 10])
    return n_str if (n_str := str(n)) else ""
def norm(s):
    # STT writes numbers as digits ("6.30", "15%", "$87"); compare them as words.
    s = s.lower()
    s = re.sub(r"\$(\d+)", lambda m: m[1] + " dollars", s)
    s = s.replace("%", " percent").replace("per cent", "percent")
    s = re.sub(r"(\d+)[.:](\d\d)", lambda m: m[1] + " " + m[2], s)
    s = re.sub(r"\d+", lambda m: " " + say(m[0]) + " ", s)
    return re.sub(r"[^a-z0-9' ]", " ", s).split()
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
