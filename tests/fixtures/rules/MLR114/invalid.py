from manim import *


class Bad(Scene):
    def construct(self):
        path = VMobject()
        path.set_points_as_corners([[0, 0], [1, 1], [2, 0]])
        line = VMobject()
        line.set_points([(0.0, 0.0, 0.0, 1.0), (1.0, 2.0, 3.0, 4.0)])
        self.add(path, line)
