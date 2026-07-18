from manim import *


class ZigZag(VMobject):
    def generate_points(self):
        self.start_new_path(ORIGIN)
        self.add_line_to(RIGHT)
        return self

    def cubic_curves(self):
        return self.points.reshape((-1, 4, 3))

    def cubic_anchors(self):
        return self.points[0::4]

    def quadratic_curves(self):
        return self.points.reshape(-1, 3, 3)
