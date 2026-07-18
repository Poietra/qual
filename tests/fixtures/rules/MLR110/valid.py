from manim import *


class FineScene(Scene):
    def construct(self):
        ok = MathTex(r"\frac{a}{b}")
        # Braces spread over arguments are judged jointly.
        spread = MathTex(r"e^{i", r"\tau} = 1")
        # Pure count imbalances are repaired by Manim before compiling.
        repaired = MathTex(r"\frac{1}{")
        # Macros, comments, and verbatim are outside the literal subset.
        macro = MathTex(r"\def\x{} }")
        comment = Tex(r"100% } sure")
        matched = MathTex(r"\begin{cases} x \end{cases}")
        self.add(ok, spread, repaired, macro, comment, matched)
