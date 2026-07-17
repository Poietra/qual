from manim import *


class Good(Scene):
    def construct(self, size):
        a = Text("hello", font_size=48)
        b = Text("hello", font_size=0.5)
        c = MathTex("x", font_size=size)
        d = Tex("y")
        e = custom_label("z", font_size=0)


def custom_label(text, font_size):
    return text
