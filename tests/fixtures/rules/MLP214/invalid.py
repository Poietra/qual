from manim import *


class Demo(Scene):
    def construct(self):
        first = MathTex(r"\int_0^1 x^2 \, dx")
        second = MathTex(r"\sum_{n=1}^{\infty} \frac{1}{n^2}")
        third = MathTex(r"e^{i\pi} + 1 = 0")
        fourth = Tex(r"Euler's identity")
        again = MathTex(r"\int_0^1 x^2 \, dx")
        self.play(
            Write(first),
            Write(second),
            Write(third),
            Write(fourth),
            Write(again),
        )
