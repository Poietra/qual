import numpy as np
import time
from math import inf as INF
from math import nan
from urllib.request import urlopen
from manim import *

nan = 3.0
limit = (scale := 2)


def helper(time):
    return time


shadow = lambda urlopen: urlopen

seeded = np.random.seed(1)
text = open("data.txt").read_text()
