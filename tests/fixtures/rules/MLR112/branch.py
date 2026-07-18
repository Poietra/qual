from manim import *


class ShapeFromConfig(VMobject):
    def regroup(self, shape):
        # The shape is not a literal: Unknown, silence.
        return self.points.reshape(shape)
