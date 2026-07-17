from manim import *


class Bad(Scene):
    def construct(self):
        one = MathTex("\frac{a}{b}")
        two = Tex("x \times y")
        both = MathTex("\alpha + \tau")
        supp = MathTex("\begin{align}x\end{align}")  # manim-lint: ignore[MLR103]
