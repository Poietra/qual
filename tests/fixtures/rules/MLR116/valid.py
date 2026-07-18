from manim import *


class StartedPathScene(Scene):
    def construct(self):
        path = VMobject()
        path.start_new_path(ORIGIN)
        path.add_line_to([1, 0, 0])
        self.add(path)

        # A Square starts with a non-empty own path.
        square = Square()
        square.add_line_to([2, 0, 0])
        self.add(square)

        # close_path after the path has started is fine.
        loop = VMobject()
        loop.start_new_path(ORIGIN)
        loop.close_path()
        self.add(loop)
