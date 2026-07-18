from manim import *


class EmptyPathScene(Scene):
    def construct(self):
        path = VMobject()
        path.add_line_to([1, 0, 0])
        self.add(path)

        loop = VMobject()
        loop.close_path()
        self.add(loop)
