from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        group = VGroup(square, dot)
        self.add(group)
        self.remove(dot)
        self.play(FadeIn(group))


class Direct(Scene):
    def construct(self):
        circle = Circle()
        label = Dot()
        panel = VGroup(circle, label)
        self.add(panel)
        self.remove(label)
        self.add(panel)
