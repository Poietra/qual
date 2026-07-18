from manim import *


class FormulaScene(Scene):
    def construct(self):
        broken = MathTex(r"a}b{c")
        crossed = MathTex(r"\begin{cases} x \end{matrix}")
        unclosed = Tex(r"\begin{tabular} y")
        joint = MathTex(r"} closes", r"{ opens")
        self.add(broken, crossed, unclosed, joint)
