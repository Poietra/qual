import numpy as np
from manim import *


class Good(Scene):
    def construct(self):
        path = VMobject()
        # Proper (N, 3) rows.
        path.set_points_as_corners([[0, 0, 0], [1, 1, 0], [-1, 0.5, 0]])
        # Empty literal: nothing to judge.
        path.append_points([])
        # Rows through a name are not a judged literal.
        rows = [[0, 0], [1, 1]]
        path.set_points_smoothly(rows)
        # Direction-constant rows are names, not numeric literals.
        path.set_points_as_corners([UP, DOWN])
        # A wrapped call is not a judged literal.
        path.set_points(np.array([[0, 0], [1, 1]]))
        self.add(path)
