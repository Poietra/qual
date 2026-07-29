from manim import *


class Bad(Scene):
    def construct(self):
        a = Text("hello", font_size=0)
        b = MarkupText("plain", font_size=-12)
        c = MathTex("x", font_size=0.0)
        d = Tex("y", font_size=-1.5)
        e = Text("zero", font_size=0)  # qual: ignore[MLR115]
