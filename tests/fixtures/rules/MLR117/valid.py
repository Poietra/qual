from manim.mobject.text.text_mobject import register_font

from manim import *


class Good(Scene):
    def construct(self):
        with register_font("fonts/custom.ttf"):
            title = Text("hello", font="Custom Font")
            self.add(title)
        handle = register_font("fonts/other.ttf")
        stack_push(register_font("fonts/third.ttf"))


def stack_push(context):
    return context
