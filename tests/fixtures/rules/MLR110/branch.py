from manim import *
from data import fetch_suffix


class DynamicScene(Scene):
    def construct(self):
        # A non-literal part makes the joined expression Unknown.
        formula = MathTex(r"a}b{c" + fetch_suffix())
        # The isolate machinery rewrites the joined string: Unknown.
        isolated = MathTex(r"a}b{c", substrings_to_isolate=["a"])
        self.add(formula, isolated)
