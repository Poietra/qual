from manim import *


class Good(Scene):
    def construct(self, x):
        raw = MathTex(r"\frac{a}{b}")
        escaped = MathTex("\\frac{a}{b}")
        plain = Tex("plain text")
        not_a_command = MathTex("\tomato")
        newline = Tex("a + b \n c")
        formatted = MathTex(f"\\frac{x}")
        keyword_only = MathTex("x", arg_separator="\t")
