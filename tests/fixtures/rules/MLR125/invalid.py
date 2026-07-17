from manim import *


class Bad(Scene):
    def construct(self):
        anchor = Mobject()
        self.add(anchor)
        marker = Mobject()
        self.add(marker)
        self.wait()
