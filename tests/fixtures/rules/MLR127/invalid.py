from manim import *


class Bad(Scene):
    def construct(self):
        eq = MathTex("a^2", "+", "b^2")
        eq.set_color_by_tex("c^2", RED)
        part = eq.get_part_by_tex("x")
        title = Tex("hello world")
        title.get_part_by_tex("goodbye")
        self.add(eq, title)
